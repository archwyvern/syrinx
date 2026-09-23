//! A source, its layers and its mix stage composed: pre-mixed blocks in order. What `render`
//! drains and the C ABI wraps.

use crate::{Block, Error, RenderOptions};

use super::Deadline;
use super::mixer::Mixer;
use super::source::{Source, plan};
use super::stem::Stem;
use super::wrapper::block_len;

/// A [`Source`], its layers and its mix stage composed: pre-mixed blocks, in order. What
/// `render` drains and the C ABI wraps. Dropping it stops every isolate and joins every thread.
#[derive(Debug)]
pub struct Stream {
    pub(super) source: Source,
    pub(super) stems: Vec<Stem>,
    pub(super) mixer: Option<Mixer>,
    pub(super) whole: Option<Vec<f32>>,
    pub(super) cursor: usize,
    pub(super) streaming: bool,
    pub(super) error: Option<Error>,
}

impl Stream {
    /// Opens the source and starts its layers and mix stage per `opts.target`.
    pub fn open(source: &str, name: &str, opts: &RenderOptions) -> Result<Stream, Error> {
        Self::open_with(source, name, opts, Deadline::new(opts.timeout))
    }

    pub(super) fn open_with(text: &str, name: &str, opts: &RenderOptions, deadline: Deadline) -> Result<Stream, Error> {
        let source = Source::open_with(text, name, opts, deadline)?;
        let (selected, use_default) = plan(&source.info, &opts.target)?;
        let names: Vec<&str> = selected.iter().map(String::as_str).collect();
        let mut stems = source.stems_with(&names, deadline)?;
        // One layer and nothing to combine it with: the sum of one buffer is that buffer, and
        // there is no arithmetic to get wrong. This keeps a sound effect at exactly one isolate
        // beyond the inspection.
        let mut mixer =
            if selected.len() == 1 && !use_default { None } else { Some(source.mixer_with(&opts.target, deadline)?) };
        let any_live = stems.iter().any(Stem::streaming);
        let mix_streams = mixer.as_ref().is_none_or(Mixer::streaming);
        let whole = match mixer.as_mut() {
            Some(m) if !m.streaming() => {
                let layers = drain_round_robin(&mut stems, source.channels())?;
                let refs: Vec<&[f32]> = layers.iter().map(Vec::as_slice).collect();
                Some(m.mix_all_with(&refs, deadline.remaining())?.samples)
            }
            _ => None,
        };
        Ok(Stream { source, stems, mixer, whole, cursor: 0, streaming: any_live && mix_streams, error: None })
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// True when at least one layer streams and the mix stage streams: nothing was rendered
    /// whole at open. False: the whole sound exists after open and is handed out in blocks.
    pub fn streaming(&self) -> bool {
        self.streaming
    }

    /// The next block, interleaved, or `None` after the last. An error is sticky.
    pub fn next_block(&mut self) -> Result<Option<Block>, Error> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let result = self.pull();
        if let Err(e) = &result {
            self.error = Some(e.clone());
        }
        result
    }

    fn pull(&mut self) -> Result<Option<Block>, Error> {
        let frames = self.source.frames();
        let channels = self.source.channels();
        if self.cursor >= frames {
            return Ok(None);
        }
        let offset = self.cursor;
        let n = block_len(offset, frames);
        if let Some(whole) = &self.whole {
            let ch = channels as usize;
            let samples = whole[offset * ch..(offset + n) * ch].to_vec();
            self.cursor += n;
            return Ok(Some(Block { offset, frames: n, samples }));
        }
        let mut blocks = Vec::with_capacity(self.stems.len());
        for stem in &mut self.stems {
            match stem.next_block()? {
                Some(block) if block.offset == offset && block.frames == n => blocks.push(block.samples),
                Some(block) => {
                    return Err(Error::internal(format!(
                        "layer \"{}\" produced the block at frame {} when frame {offset} was due",
                        stem.name(),
                        block.offset
                    )));
                }
                None => return Err(Error::internal(format!("layer \"{}\" ended at frame {offset}", stem.name()))),
            }
        }
        let block = match self.mixer.as_mut() {
            Some(mixer) => {
                let refs: Vec<&[f32]> = blocks.iter().map(Vec::as_slice).collect();
                mixer.mix(offset, &refs)?
            }
            None => Block { offset, frames: n, samples: blocks.pop().expect("one layer") },
        };
        self.cursor += n;
        Ok(Some(block))
    }
}

/// Every layer to the end, one block from each in turn, so their threads stay busy together.
fn drain_round_robin(stems: &mut [Stem], channels: u32) -> Result<Vec<Vec<f32>>, Error> {
    let mut out: Vec<Vec<f32>> = stems.iter().map(|s| Vec::with_capacity(s.frames * channels as usize)).collect();
    loop {
        let mut any = false;
        for (stem, samples) in stems.iter_mut().zip(out.iter_mut()) {
            if let Some(block) = stem.next_block()? {
                samples.extend_from_slice(&block.samples);
                any = true;
            }
        }
        if !any {
            return Ok(out);
        }
    }
}
