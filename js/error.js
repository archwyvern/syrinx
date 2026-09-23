// The error every host throws, in one module of its own so the browser host can share it without
// importing the Node host.

/** Thrown for a source that will not compile, will not run, or breaks the contract. */
export class SyrinxError extends Error {
  constructor(kind, message, file, line, column) {
    super(message);
    this.name = "SyrinxError";
    /** "check" | "compile" | "runtime" | "timeout" | "contract" | "internal" */
    this.kind = kind;
    /** The module the position refers to, or null. */
    this.file = file ?? null;
    /** 1-based; 0 when unknown. */
    this.line = line ?? 0;
    this.column = column ?? 0;
  }
}
