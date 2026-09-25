// Incremental parser for a text/event-stream body read through fetch().
//
// Not EventSource: it hides HTTP status codes (401/403) and retries on its own,
// and the side panel needs to tell "logged out" from "server down".
//
// Follows the HTML spec's line rules: lines end in CRLF, LF or CR (a CRLF may be
// split across chunks), ":" lines are comments (keep-alives), `data:` lines of
// one event are joined with "\n", and a blank line dispatches the event.

export class SseParser {
  #buf = "";
  #data = [];
  #event = "";
  // The previous chunk ended in "\r": a leading "\n" now belongs to that CRLF.
  #pendingCR = false;

  /** Feed decoded text; returns the events completed by it as {event, data}. */
  push(chunk) {
    const out = [];
    if (chunk === "") return out; // keep a pending CR waiting for its LF
    if (this.#pendingCR && chunk.startsWith("\n")) chunk = chunk.slice(1);
    this.#pendingCR = false;
    this.#buf += chunk;

    let start = 0;
    for (let i = 0; i < this.#buf.length; i++) {
      const c = this.#buf[i];
      if (c !== "\n" && c !== "\r") continue;
      this.#line(this.#buf.slice(start, i), out);
      if (c === "\r") {
        if (i + 1 < this.#buf.length) {
          if (this.#buf[i + 1] === "\n") i++;
        } else {
          this.#pendingCR = true;
        }
      }
      start = i + 1;
    }
    this.#buf = this.#buf.slice(start);
    return out;
  }

  #line(line, out) {
    if (line === "") {
      if (this.#data.length) out.push({ event: this.#event || "message", data: this.#data.join("\n") });
      this.#data = [];
      this.#event = "";
      return;
    }
    if (line.startsWith(":")) return;
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    let value = colon === -1 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "data") this.#data.push(value);
    else if (field === "event") this.#event = value;
    // id / retry / unknown fields are not used.
  }
}
