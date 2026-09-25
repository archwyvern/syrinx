//! The session: the playlist and what is playing, on a thread of its own. The window sends it
//! [`Action`]s and draws the [`View`] it publishes. Everything that decides what plays happens
//! here, on a tick that runs whether or not the window is being painted: a track ending and the
//! next one starting, a source changing on disk, a later launch handing over files, the device
//! going away. A minimised window on Wayland gets no frames at all, so a player that did this
//! from the window's frame stopped at the end of the track it was on.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::cache::{self, Cache};
use crate::instance::Request;
use crate::mixer::{Checkpoints, Command, Gains, Meters, MixerHandle, Status};
use crate::output::{Audio, DeviceInfo, OnError, OutputInfo, Shared};
use crate::playlist::{self, Inspected, Playlist, RowId};
use crate::track::Track;
use crate::watch::Watch;

/// How often the session looks at the playhead while a track plays: the longest a finished
/// track waits before the next one starts.
const PLAYING_TICK: Duration = Duration::from_millis(20);
/// How often it looks for handed-over files, inspections and edits otherwise.
const IDLE_TICK: Duration = Duration::from_millis(100);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Settings {
    pub volume: f32,
    pub loop_track: bool,
    /// `None` = the default device.
    pub device: Option<String>,
    pub reload_on_change: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { volume: 0.8, loop_track: false, device: None, reload_on_change: true }
    }
}

pub enum Action {
    /// Sources or folders of them, appended; `replace` empties the list first (launch
    /// arguments do).
    Add {
        paths: Vec<PathBuf>,
        replace: bool,
    },
    /// Loads the row and plays it from the start.
    Play(RowId),
    /// Pauses or resumes what is loaded; with nothing loaded, plays `fallback` or the first row.
    TogglePlay {
        fallback: Option<RowId>,
    },
    Stop,
    Next,
    Prev,
    /// To a source frame.
    Seek(usize),
    /// By seconds, either way.
    SeekBy(f64),
    SetLoop(bool),
    SetVolume(f32),
    Remove(RowId),
    Clear,
    /// Deletes the loaded track's render and renders it again.
    Rerender,
    /// Deletes every render; the loaded track renders again.
    ClearCache,
    /// `None` = the default device.
    SetDevice(Option<String>),
    SetReloadOnChange(bool),
    /// Lists the devices again, for the picker.
    ListDevices,
    Shutdown,
}

/// The session as the window draws it, published whole on every change. The window takes the
/// latest at the start of a frame and lets it go at the end, so a window that is not being
/// painted keeps nothing alive: a track the session lets go is gone.
pub struct View {
    pub playlist: Playlist,
    pub track: Option<Arc<Track>>,
    /// The loaded track's faders. The window writes them; the mixer reads them per chunk.
    pub gains: Option<Arc<Gains>>,
    pub output: Option<OutputInfo>,
    pub devices: Vec<DeviceInfo>,
    pub settings: Settings,
    /// The latest thing worth saying, and when it was said.
    pub note: Option<(String, Instant)>,
    /// Counts the later launches that handed files over; the window comes forward when it moves.
    pub handovers: u64,
    pub shared: Arc<Shared>,
    pub mixer: Arc<Status>,
    /// Each layer's level in the chunk being heard.
    pub meters: Arc<Meters>,
    checkpoints: Arc<Checkpoints>,
    seek_target: Option<usize>,
}

impl View {
    /// The source frame under the playhead.
    pub fn position(&self) -> Option<usize> {
        let track = self.track.as_ref()?;
        Some(position(track, &self.shared, &self.checkpoints, self.output.as_ref(), self.seek_target))
    }

    /// A track is loaded and not paused.
    pub fn playing(&self) -> bool {
        self.track.is_some() && self.shared.playing.load(Ordering::Relaxed)
    }
}

/// The source frame under the playhead: the callback's frame count through the mixer's
/// checkpoints, or the seek target while nothing has been pushed since a seek.
fn position(
    track: &Track,
    shared: &Shared,
    checkpoints: &Checkpoints,
    output: Option<&OutputInfo>,
    seek_target: Option<usize>,
) -> usize {
    let consumed = shared.consumed.load(Ordering::Relaxed);
    let device_rate = output.map_or(track.sample_rate, |o| o.sample_rate);
    match checkpoints.position(consumed, device_rate, track.sample_rate) {
        Some(p) => p.min(track.frames),
        None => seek_target.unwrap_or(0),
    }
}

/// What the session starts from.
pub struct Start {
    pub cache: Cache,
    pub settings: Settings,
    /// Launch arguments: they fill the playlist and the first of them plays.
    pub paths: Vec<PathBuf>,
    /// Hand-overs from later launches.
    pub requests: Receiver<Request>,
    /// Silent at the device whatever the volume (a screenshot run).
    pub muted: bool,
}

pub struct SessionHandle {
    actions: Sender<Action>,
    view: Arc<Mutex<Arc<View>>>,
    join: Option<JoinHandle<()>>,
}

impl SessionHandle {
    /// Starts the session. `audio` runs on the session's thread and the device it opens stays
    /// there; `repaint` is called whenever there is something new to draw.
    pub fn spawn(
        start: Start,
        audio: impl FnOnce() -> Box<dyn Audio> + Send + 'static,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> SessionHandle {
        let Start { cache, settings, paths, requests, muted } = start;
        let shared = Shared::new(settings.volume);
        shared.muted.store(muted, Ordering::Relaxed);
        // The mixer starts on a ring nobody reads. The session thread opens the device and
        // hands the mixer the real one, so the window never waits on a sound server.
        let (idle, _) = rtrb::RingBuffer::<f32>::new(1);
        let mixer = MixerHandle::spawn(idle, Arc::clone(&shared), 48_000, 2);
        mixer.send(Command::Loop(settings.loop_track));
        let mut playlist = Playlist::default();
        let first = playlist.replace(&paths);
        // The first view exists before the thread does, so the window's first frame shows the list.
        let view = Arc::new(Mutex::new(Arc::new(View {
            playlist: playlist.clone(),
            track: None,
            gains: None,
            output: None,
            devices: Vec::new(),
            settings: settings.clone(),
            note: None,
            handovers: 0,
            shared: Arc::clone(&shared),
            mixer: Arc::clone(&mixer.status),
            meters: Arc::clone(&mixer.meters),
            checkpoints: Arc::clone(&mixer.checkpoints),
            seek_target: None,
        })));
        let (actions, rx) = mpsc::channel();
        let published = Arc::clone(&view);
        let join = std::thread::Builder::new()
            .name("syrinx-player-session".into())
            .spawn(move || {
                let mut session = Session {
                    actions: rx,
                    requests,
                    view: published,
                    repaint: Arc::new(repaint),
                    cache,
                    settings,
                    playlist,
                    inspects: Vec::new(),
                    audio: audio(),
                    shared,
                    output: None,
                    output_error: Arc::new(Mutex::new(None)),
                    mixer,
                    track: None,
                    gains: None,
                    watch: None,
                    devices: Vec::new(),
                    note: None,
                    seek_target: None,
                    handovers: 0,
                    dirty: true,
                };
                session.start(first);
                session.run();
            })
            .expect("spawn the session thread");
        SessionHandle { actions, view, join: Some(join) }
    }

    pub fn send(&self, action: Action) {
        let _ = self.actions.send(action);
    }

    /// The latest view. Take one a frame and keep none between frames.
    pub fn view(&self) -> Arc<View> {
        Arc::clone(&self.view.lock().unwrap())
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.send(Action::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

struct Session {
    actions: Receiver<Action>,
    requests: Receiver<Request>,
    view: Arc<Mutex<Arc<View>>>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    cache: Cache,
    settings: Settings,
    playlist: Playlist,
    inspects: Vec<Receiver<Inspected>>,
    audio: Box<dyn Audio>,
    shared: Arc<Shared>,
    output: Option<OutputInfo>,
    output_error: Arc<Mutex<Option<String>>>,
    mixer: MixerHandle,
    track: Option<Arc<Track>>,
    gains: Option<Arc<Gains>>,
    watch: Option<Watch>,
    devices: Vec<DeviceInfo>,
    note: Option<(String, Instant)>,
    /// Where the playhead is meant to be while no chunk has been pushed since a seek.
    seek_target: Option<usize>,
    handovers: u64,
    /// Something the window shows has changed since the last publish.
    dirty: bool,
}

impl Session {
    fn start(&mut self, first: Vec<usize>) {
        let wanted = self.settings.device.clone();
        self.switch_device(wanted);
        self.queue_inspect(&first);
        if let Some(&i) = first.first() {
            self.play(i);
        }
    }

    fn run(&mut self) {
        loop {
            if self.dirty {
                self.publish();
            }
            let tick = if self.track.is_some() && self.shared.playing.load(Ordering::Relaxed) {
                PLAYING_TICK
            } else {
                IDLE_TICK
            };
            match self.actions.recv_timeout(tick) {
                Ok(Action::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(action) => self.apply(action),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
        self.mixer.send(Command::Unload);
        self.release_track();
    }

    fn publish(&mut self) {
        let view = View {
            playlist: self.playlist.clone(),
            track: self.track.clone(),
            gains: self.gains.clone(),
            output: self.output.clone(),
            devices: self.devices.clone(),
            settings: self.settings.clone(),
            note: self.note.clone(),
            handovers: self.handovers,
            shared: Arc::clone(&self.shared),
            mixer: Arc::clone(&self.mixer.status),
            meters: Arc::clone(&self.mixer.meters),
            checkpoints: Arc::clone(&self.mixer.checkpoints),
            seek_target: self.seek_target,
        };
        *self.view.lock().unwrap() = Arc::new(view);
        self.dirty = false;
        (self.repaint)();
    }

    fn say(&mut self, text: impl Into<String>) {
        self.note = Some((text.into(), Instant::now()));
        self.dirty = true;
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Add { paths, replace } => self.add_paths(&paths, replace),
            Action::Play(id) => {
                if let Some(i) = self.playlist.index_of(id) {
                    self.play(i);
                }
            }
            Action::TogglePlay { fallback } => self.toggle_play(fallback),
            Action::Stop => self.stop(),
            Action::Next => {
                if let Some(i) = self.playlist.next() {
                    self.play(i);
                }
            }
            Action::Prev => {
                if let Some(i) = self.playlist.prev() {
                    self.play(i);
                }
            }
            Action::Seek(frame) => self.seek(frame),
            Action::SeekBy(seconds) => self.seek_by(seconds),
            Action::SetLoop(on) => {
                self.settings.loop_track = on;
                self.mixer.send(Command::Loop(on));
            }
            Action::SetVolume(volume) => {
                self.shared.set_volume(volume);
                self.settings.volume = self.shared.volume();
            }
            Action::Remove(id) => self.remove(id),
            Action::Clear => {
                self.stop();
                self.playlist.clear();
            }
            Action::Rerender => self.rerender(),
            Action::ClearCache => self.clear_cache(),
            Action::SetDevice(name) => self.switch_device(name),
            Action::SetReloadOnChange(on) => {
                self.settings.reload_on_change = on;
                self.watch = if on { self.watch_track() } else { None };
            }
            Action::ListDevices => self.devices = self.audio.devices(),
            Action::Shutdown => {}
        }
        self.dirty = true;
    }

    /// What the session does on its own: inspections landing, hand-overs, a dead device, an
    /// edit to the loaded source, the loaded track ending.
    fn tick(&mut self) {
        self.poll_inspects();

        while let Ok(request) = self.requests.try_recv() {
            self.add_paths(&request.paths, request.replace);
            self.handovers += 1;
            self.dirty = true;
        }

        let device_error = self.output_error.lock().unwrap().take();
        if let Some(e) = device_error {
            self.say(format!("audio device: {e}"));
            self.output = None;
            self.switch_device(None);
        }

        if self.watch.as_ref().is_some_and(|w| w.changed()) && self.settings.reload_on_change {
            self.reload();
        }

        // A track that ended (or died) while not looping: the next one, or rewind at the end
        // of the list.
        if let Some(track) = self.track.clone() {
            let at_end = self.mixer.status.at_end.load(Ordering::Relaxed);
            let finished_at = self.shared.finished_at.load(Ordering::Relaxed);
            let consumed = self.shared.consumed.load(Ordering::Relaxed);
            if at_end && consumed >= finished_at && !self.settings.loop_track {
                let error = self.mixer.status.error.lock().unwrap().clone().or_else(|| track.failed());
                if let (Some(i), Some(e)) = (self.playlist.current, error) {
                    self.playlist.rows[i].error = Some(e);
                }
                match self.playlist.next() {
                    Some(next) => self.play(next),
                    None => {
                        self.shared.playing.store(false, Ordering::Relaxed);
                        self.seek(0);
                    }
                }
                self.dirty = true;
            }
        }
    }

    // ------------------------------------------------------------------ playlist and tracks

    fn queue_inspect(&mut self, indices: &[usize]) {
        if indices.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = indices.iter().map(|&i| self.playlist.rows[i].path.clone()).collect();
        self.inspects.push(playlist::inspect_rows(paths));
    }

    fn poll_inspects(&mut self) {
        let mut done = Vec::new();
        for (k, rx) in self.inspects.iter().enumerate() {
            loop {
                match rx.try_recv() {
                    Ok(found) => {
                        // Rows may have moved since the inspection was queued: match by path.
                        if let Some(row) = self.playlist.rows.iter_mut().find(|r| r.path == found.path) {
                            match found.result {
                                Ok((name, duration, stems)) => {
                                    // A source that declares no name keeps its file's stem.
                                    if let Some(name) = name {
                                        row.name = name;
                                    }
                                    row.duration = Some(duration);
                                    row.stems = stems;
                                    row.error = None;
                                }
                                Err(e) => row.error = Some(e),
                            }
                            self.dirty = true;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        done.push(k);
                        break;
                    }
                }
            }
        }
        for k in done.into_iter().rev() {
            self.inspects.remove(k);
        }
    }

    fn add_paths(&mut self, paths: &[PathBuf], replace: bool) {
        let added = if replace { self.playlist.replace(paths) } else { self.playlist.append(paths) };
        let first = added.first().copied();
        self.queue_inspect(&added);
        if replace {
            self.stop();
            if let Some(i) = first {
                self.play(i);
            }
        } else if self.track.is_none()
            && let Some(i) = first
        {
            self.play(i);
        }
    }

    /// Loads row `i` and plays it from the start; a row that fails is marked and skipped, so a
    /// playlist plays through around a broken file.
    fn play(&mut self, i: usize) {
        let mut i = i;
        for _ in 0..self.playlist.rows.len().max(1) {
            if i >= self.playlist.rows.len() {
                return;
            }
            match self.load(i, 0) {
                Ok(()) => return,
                Err(e) => {
                    self.playlist.rows[i].error = Some(format!("{e:#}"));
                    i += 1;
                }
            }
        }
        self.stop();
    }

    /// Loads row `i` at `start_frame`. Faders carry over from the loaded track when it is the
    /// same file (a reload), by layer name.
    fn load(&mut self, i: usize, start_frame: usize) -> Result<()> {
        let carry = self.track.clone().zip(self.gains.clone());
        self.load_with(i, start_frame, carry)
    }

    fn load_with(&mut self, i: usize, start_frame: usize, carry: Option<(Arc<Track>, Arc<Gains>)>) -> Result<()> {
        let path = self.playlist.rows[i].path.clone();
        let repaint = Arc::clone(&self.repaint);
        let track = Track::open(&self.cache, &path, move || repaint())?;
        let gains = Arc::new(Gains::new(track.stem_names.len()));
        if let Some((old_track, old_gains)) = carry
            && old_track.path == track.path
        {
            for (n, name) in track.stem_names.iter().enumerate() {
                if let Some(o) = old_track.stem_names.iter().position(|s| s == name) {
                    gains.set_gain(n, old_gains.gain(o));
                    gains.set_mute(n, old_gains.muted(o));
                    gains.set_solo(n, old_gains.soloed(o));
                }
            }
        }
        self.release_track();
        // The mixer resets these too, but not before its thread gets to the command; until then
        // the previous track's end would read as this one's and advance the playlist again.
        self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
        self.mixer.status.at_end.store(false, Ordering::Relaxed);
        self.mixer.send(Command::Load { track: Arc::clone(&track), gains: Arc::clone(&gains), start_frame });
        self.shared.playing.store(true, Ordering::Relaxed);
        self.seek_target = Some(start_frame);
        let row = &mut self.playlist.rows[i];
        row.name = track.name.clone();
        row.duration = Some(track.duration);
        row.stems = track.stem_names.clone();
        row.error = None;
        self.playlist.current = Some(i);
        // A long session renders track after track; trim as it goes, never the one playing.
        if let Err(e) = self.cache.evict(cache::CACHE_CAP_BYTES, &[track.dir.as_path()]) {
            eprintln!("warning: trimming the render cache: {e:#}");
        }
        self.track = Some(track);
        self.gains = Some(gains);
        self.watch = if self.settings.reload_on_change { self.watch_track() } else { None };
        Ok(())
    }

    /// Lets go of the loaded track and stops its render. The render threads hold the track
    /// themselves, so dropping the handle alone would leave them rendering a track nobody plays.
    fn release_track(&mut self) {
        if let Some(track) = self.track.take() {
            track.cancel();
        }
        self.gains = None;
    }

    /// A watch on the loaded track's source and imports; a note when there cannot be one.
    fn watch_track(&mut self) -> Option<Watch> {
        let track = self.track.as_ref()?;
        let mut closure = track.dependencies.clone();
        closure.push(track.path.clone());
        match Watch::new(closure) {
            Ok(watch) => Some(watch),
            Err(e) => {
                self.say(format!("not watching for changes: {e:#}"));
                None
            }
        }
    }

    fn stop(&mut self) {
        self.shared.playing.store(false, Ordering::Relaxed);
        self.mixer.send(Command::Unload);
        self.release_track();
        self.watch = None;
        self.seek_target = None;
        self.playlist.current = None;
    }

    fn remove(&mut self, id: RowId) {
        let Some(i) = self.playlist.index_of(id) else {
            return;
        };
        if self.playlist.current == Some(i) {
            self.stop();
        }
        self.playlist.remove(i);
    }

    fn reload(&mut self) {
        let Some(i) = self.playlist.current else {
            return;
        };
        let position = self.position();
        let was_playing = self.shared.playing.load(Ordering::Relaxed);
        eprintln!(
            "reload: {} changed, resuming at {}",
            self.playlist.rows[i].path.display(),
            crate::app::fmt_time(position as f64 / self.track.as_ref().map_or(48_000.0, |t| t.sample_rate as f64))
        );
        if let Err(e) = self.load(i, position) {
            self.playlist.rows[i].error = Some(format!("{e:#}"));
            self.say(format!("{e:#}"));
            self.shared.playing.store(false, Ordering::Relaxed);
            return;
        }
        self.shared.playing.store(was_playing, Ordering::Relaxed);
    }

    fn rerender(&mut self) {
        let (Some(track), Some(i)) = (self.track.clone(), self.playlist.current) else {
            return;
        };
        let position = self.position();
        let carry = self.track.clone().zip(self.gains.clone());
        self.mixer.send(Command::Unload);
        self.release_track();
        let _ = fs::remove_dir_all(&track.dir);
        drop(track);
        if let Err(e) = self.load_with(i, position, carry) {
            self.playlist.rows[i].error = Some(format!("{e:#}"));
            self.say(format!("{e:#}"));
        }
    }

    fn clear_cache(&mut self) {
        match self.cache.clear() {
            Ok(()) => self.say("render cache cleared"),
            Err(e) => self.say(format!("clearing the cache: {e:#}")),
        }
        // The loaded track's render went with the rest.
        self.rerender();
    }

    fn toggle_play(&mut self, fallback: Option<RowId>) {
        if self.track.is_some() {
            let at_end = self.mixer.status.at_end.load(Ordering::Relaxed);
            if at_end && !self.settings.loop_track {
                self.seek(0);
                self.shared.playing.store(true, Ordering::Relaxed);
            } else {
                self.shared.playing.fetch_xor(true, Ordering::Relaxed);
            }
        } else if let Some(i) =
            fallback.and_then(|id| self.playlist.index_of(id)).or_else(|| (!self.playlist.rows.is_empty()).then_some(0))
        {
            self.play(i);
        }
    }

    fn seek(&mut self, frame: usize) {
        if let Some(track) = &self.track {
            let frame = frame.min(track.frames);
            self.shared.finished_at.store(u64::MAX, Ordering::Relaxed);
            self.mixer.status.at_end.store(false, Ordering::Relaxed);
            self.mixer.send(Command::Seek(frame));
            self.seek_target = Some(frame);
        }
    }

    fn seek_by(&mut self, seconds: f64) {
        if let Some(track) = &self.track {
            let target = (self.position() as f64 + seconds * track.sample_rate as f64).max(0.0) as usize;
            self.seek(target);
        }
    }

    fn position(&self) -> usize {
        self.track.as_ref().map_or(0, |track| {
            position(track, &self.shared, &self.mixer.checkpoints, self.output.as_ref(), self.seek_target)
        })
    }

    // ------------------------------------------------------------------ the device

    /// Opens `name` (the default when `None`) and hands the mixer its ring. The wanted device
    /// falls back to the default with a note; when nothing opens, whatever was open stays.
    fn switch_device(&mut self, name: Option<String>) {
        let opened = match self.audio.open(name.as_deref(), &self.shared, self.on_error()) {
            Ok(opened) => Some(opened),
            Err(first) => match name {
                Some(_) => match self.audio.open(None, &self.shared, self.on_error()) {
                    Ok(opened) => {
                        self.say(format!("{first:#}; using the default device"));
                        Some(opened)
                    }
                    Err(_) => {
                        self.say(format!("no audio output: {first:#}"));
                        None
                    }
                },
                None => {
                    self.say(format!("no audio output: {first:#}"));
                    None
                }
            },
        };
        if let Some((info, producer)) = opened {
            self.mixer.send(Command::Output { producer, sample_rate: info.sample_rate, channels: info.channels });
            self.output = Some(info);
            self.settings.device = name;
        }
    }

    fn on_error(&self) -> OnError {
        let slot = Arc::clone(&self.output_error);
        Box::new(move |e| *slot.lock().unwrap() = Some(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    /// A device that is a ring and nothing else: the test plays the callback's part.
    struct Loopback {
        consumers: Sender<rtrb::Consumer<f32>>,
    }

    impl Audio for Loopback {
        fn open(&mut self, _: Option<&str>, _: &Arc<Shared>, _: OnError) -> Result<(OutputInfo, rtrb::Producer<f32>)> {
            let (producer, consumer) = rtrb::RingBuffer::<f32>::new(48_000 * 2);
            self.consumers.send(consumer).unwrap();
            Ok((OutputInfo { device_name: "loopback".into(), sample_rate: 48_000, channels: 2 }, producer))
        }

        fn devices(&mut self) -> Vec<DeviceInfo> {
            Vec::new()
        }
    }

    /// Stands in for the audio callback until dropped: honours a flush, plays silence while
    /// paused, and takes what the ring holds, counting it as played. `speed` is the playback
    /// rate against real time: `None` takes the ring as fast as the mixer fills it, so a test
    /// hears a track end in seconds; `Some(1.0)` is a real device's pace.
    struct Callback {
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    }

    impl Callback {
        fn start(consumers: Receiver<rtrb::Consumer<f32>>, shared: Arc<Shared>, speed: Option<f64>) -> Callback {
            let stop = Arc::new(AtomicBool::new(false));
            let flag = Arc::clone(&stop);
            let join = std::thread::spawn(move || {
                let mut consumer = consumers.recv().unwrap();
                let started = Instant::now();
                let mut taken: u64 = 0;
                while !flag.load(Ordering::Relaxed) {
                    if let Ok(newer) = consumers.try_recv() {
                        consumer = newer;
                    }
                    if shared.flush.load(Ordering::Acquire) {
                        let n = consumer.slots();
                        if let Ok(chunk) = consumer.read_chunk(n) {
                            chunk.commit_all();
                        }
                        shared.flush.store(false, Ordering::Release);
                    }
                    if shared.playing.load(Ordering::Relaxed) {
                        let mut n = consumer.slots();
                        if let Some(speed) = speed {
                            let due = (started.elapsed().as_secs_f64() * 48_000.0 * speed) as u64;
                            n = n.min((due.saturating_sub(taken) * 2) as usize);
                        }
                        let n = n - n % 2;
                        if n > 0 {
                            consumer.read_chunk(n).unwrap().commit_all();
                            shared.consumed.fetch_add((n / 2) as u64, Ordering::Relaxed);
                            taken += (n / 2) as u64;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            Callback { stop, join: Some(join) }
        }
    }

    impl Drop for Callback {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "syrinx-player-session-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A streaming source of `seconds` of quiet noise, `work` sines of busywork per sample to
    /// make its render take time. The directory goes into a comment, so its bytes (and so its
    /// cache key) are this test's alone and it renders from nothing.
    fn source(dir: &Path, name: &str, seconds: f64, work: u32) -> PathBuf {
        let path = dir.join(format!("{name}.syr"));
        let text = format!(
            r#"// {dir:?}
import {{ Random }} from "syrinx";
export const meta = {{ api: 4, name: "{name}", duration: {seconds} }};
export const stems = {{
  noise(ctx) {{
    const random = new Random(7);
    return (offset, frames) => {{
      const out = new Float32Array(frames);
      for (let i = 0; i < frames; i++) {{
        let busy = 0;
        for (let k = 0; k < {work}; k++) busy += Math.sin(offset + i + k);
        out[i] = random.bipolar() * 0.1 + busy * 1e-9;
      }}
      return out;
    }};
  }},
}};
"#
        );
        fs::write(&path, text).unwrap();
        path
    }

    fn session(cache: &Path, paths: Vec<PathBuf>) -> (SessionHandle, Callback) {
        session_at(cache, paths, None)
    }

    fn session_at(cache: &Path, paths: Vec<PathBuf>, speed: Option<f64>) -> (SessionHandle, Callback) {
        let (consumers_tx, consumers_rx) = mpsc::channel();
        let (_requests_tx, requests) = mpsc::channel();
        let session = SessionHandle::spawn(
            Start {
                cache: Cache::at(cache.to_path_buf()),
                settings: Settings::default(),
                paths,
                requests,
                muted: true,
            },
            move || Box::new(Loopback { consumers: consumers_tx }),
            || {},
        );
        let callback = Callback::start(consumers_rx, Arc::clone(&session.view().shared), speed);
        (session, callback)
    }

    /// Polls the session's view until `done` holds, failing after `limit`.
    fn wait_for(session: &SessionHandle, limit: Duration, what: &str, done: impl Fn(&View) -> bool) {
        let started = Instant::now();
        while !done(&session.view()) {
            assert!(started.elapsed() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The playlist advances on the session's own tick: nothing here draws a frame or asks for
    /// one, which is what a minimised window gets.
    #[test]
    fn the_next_track_starts_with_no_window_drawing() {
        let dir = temp_dir("advance");
        let first = source(&dir, "first", 1.0, 0);
        let second = source(&dir, "second", 1.0, 0);
        let (session, _callback) = session(&dir.join("cache"), vec![first, second]);
        wait_for(&session, Duration::from_secs(60), "the second track to load", |v| v.playlist.current == Some(1));
        let view = session.view();
        assert_eq!(view.track.as_ref().map(|t| t.name.as_str()), Some("second"));
        wait_for(&session, Duration::from_secs(60), "the end of the list", |v| {
            v.playlist.current == Some(1) && !v.playing()
        });
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Moving to another track stops the old one's render: its layers stop growing, well short
    /// of the end, and the files stay as they were.
    #[test]
    fn a_track_let_go_stops_rendering() {
        let dir = temp_dir("cancel");
        let long = source(&dir, "long", 600.0, 16);
        let short = source(&dir, "short", 1.0, 0);
        let (session, _callback) = session(&dir.join("cache"), vec![long, short]);
        wait_for(&session, Duration::from_secs(60), "the long track's render to start", |v| {
            v.track.as_ref().is_some_and(|t| t.stem_frontier() > 0)
        });
        let long_track = session.view().track.clone().unwrap();
        let second = session.view().playlist.rows[1].id;
        session.send(Action::Play(second));
        wait_for(&session, Duration::from_secs(60), "the short track to load", |v| v.playlist.current == Some(1));
        // A layer writer notices between blocks; give it a moment, then watch the frontier.
        std::thread::sleep(Duration::from_millis(300));
        let settled = long_track.stem_frontier();
        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(long_track.stem_frontier(), settled, "the let-go track is still rendering");
        assert!(settled < long_track.frames, "the render ran to the end before it could be stopped");
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A seek lands: backwards into what has played, and forwards into what has rendered, the
    /// playhead reads the target and moves on from there.
    #[test]
    fn a_seek_moves_the_playhead() {
        let dir = temp_dir("seek");
        let long = source(&dir, "long", 60.0, 0);
        let (session, _callback) = session_at(&dir.join("cache"), vec![long], Some(1.0));
        let rate = 48_000.0;
        wait_for(&session, Duration::from_secs(60), "playback to pass 3 s", |v| {
            v.position().is_some_and(|p| p as f64 > 3.0 * rate)
        });
        // Backwards, to 1 s.
        session.send(Action::Seek(rate as usize));
        wait_for(&session, Duration::from_secs(10), "the playhead to land near 1 s", |v| {
            v.position().is_some_and(|p| (p as f64) >= rate && (p as f64) < 2.0 * rate)
        });
        wait_for(&session, Duration::from_secs(10), "the playhead to move on from 1 s", |v| {
            v.position().is_some_and(|p| (p as f64) > 1.5 * rate && (p as f64) < 3.0 * rate)
        });
        // Forwards, to 20 s.
        session.send(Action::Seek(20 * rate as usize));
        wait_for(&session, Duration::from_secs(30), "the playhead to land near 20 s", |v| {
            v.position().is_some_and(|p| (p as f64) >= 20.0 * rate && (p as f64) < 21.0 * rate)
        });
        wait_for(&session, Duration::from_secs(30), "the playhead to move on from 20 s", |v| {
            v.position().is_some_and(|p| (p as f64) > 20.5 * rate && (p as f64) < 23.0 * rate)
        });
        drop(session);
        fs::remove_dir_all(dir).unwrap();
    }
}
