/**
 * Reading the whole flash, the way esptool does it.
 *
 * `esptool-js` ships a `readFlash`, but it accumulates with
 * `resp = appendArray(resp, packet)` — a fresh allocation and a full copy of
 * everything received so far, for every packet that arrives. Over 16 MB that is
 * quadratic: on the order of a hundred gigabytes of copying. The read slows to a
 * crawl within the first megabyte, the stub times out waiting for its
 * acknowledgement, and the connection drops. It also asks the stub for 1024
 * blocks in flight, four megabytes of unacknowledged data, and never reads the
 * MD5 the stub sends at the end.
 *
 * This reads into one preallocated buffer, keeps the in-flight window at the 64
 * blocks esptool.py uses, and checks the digest of every chunk. Everything it
 * touches on the loader is public in esptool-js's own type definitions.
 */

import { md5Hex } from './md5'

/** Bytes the stub sends per block. */
const BLOCK = 0x1000
/** Blocks the stub may have unacknowledged. esptool.py uses 64; 256 KB in flight. */
const IN_FLIGHT = 64
/**
 * Bytes per read-flash command, and so the unit that gets read again when
 * anything goes wrong.
 *
 * A browser serial port drops the odd byte: measured at roughly one packet in
 * five hundred, scattered, with no regard for what is being read. A block that
 * arrives short is unrecoverable within its command, so the only question is
 * how much has to be read a second time. At a megabyte a chunk about a third
 * of them failed and a whole sixteen-megabyte read essentially never finished;
 * at sixty-four kilobytes a chunk is sixteen blocks, almost always clean, and
 * cheap to redo when it is not.
 */
const CHUNK = 64 * 1024
/** The stub signs off each command with an MD5. */
const DIGEST_BYTES = 16

/**
 * How long to wait for one block before calling the read stalled.
 *
 * esptool.py drops its port timeout to three seconds for exactly this loop.
 * esptool-js leaves FLASH_READ_TIMEOUT at a hundred seconds, which over a
 * browser serial port turns a stall that a retry would have fixed into a
 * minute and a half of a motionless progress bar.
 */
const BLOCK_TIMEOUT = 5_000
/** A port that has said nothing for this long has finished whatever it was doing. */
const QUIET_TIMEOUT = 300
/** Attempts at one chunk before the read gives up and says so. */
const CHUNK_ATTEMPTS = 5

/**
 * The read buffer the serial port must be opened with.
 *
 * Web Serial defaults to 255 bytes, while the stub may put IN_FLIGHT blocks on
 * the wire before it waits to hear from us. Opening the port with room for the
 * whole window measurably lengthens how far a read gets before it trips, so it
 * is worth setting — but it is not on its own a cure, which is what the retry
 * below is for. Nothing outside a browser needs this: pyserial and
 * node-serialport both buffer as much as the OS will hold.
 */
export const SERIAL_BUFFER_BYTES = IN_FLIGHT * BLOCK

/** The slice of esptool-js's ESPLoader this needs, all of it publicly typed. */
export interface FlashReader {
  ESP_READ_FLASH: number
  FLASH_READ_TIMEOUT: number
  transport: { read(timeout: number): Promise<Uint8Array>; write(data: Uint8Array): Promise<void> }
  _intToByteArray(i: number): Uint8Array
  _appendArray(a: Uint8Array, b: Uint8Array): Uint8Array
  checkCommand(
    opDescription?: string,
    op?: number | null,
    data?: Uint8Array,
    chk?: number,
    responseDataLength?: number,
    timeout?: number,
  ): Promise<number | Uint8Array>
}

export class FlashReadError extends Error {}

function hex(bytes: Uint8Array): string {
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/**
 * Read `size` bytes from `addr` into one preallocated buffer.
 *
 * `onProgress` is called with the running total, at most about ten times a
 * second, so repainting never competes with draining the serial port.
 */
/**
 * Reads one command's worth of blocks into `out` at `at`. Throws on anything
 * that makes the chunk untrustworthy; the caller decides whether to try again.
 */
async function readChunk(
  loader: FlashReader,
  addr: number,
  len: number,
  out: Uint8Array,
  at: number,
  onBlock: (received: number) => void,
): Promise<void> {
  let pkt = loader._appendArray(loader._intToByteArray(addr), loader._intToByteArray(len))
  pkt = loader._appendArray(pkt, loader._intToByteArray(BLOCK))
  pkt = loader._appendArray(pkt, loader._intToByteArray(IN_FLIGHT))

  const res = await loader.checkCommand('read flash', loader.ESP_READ_FLASH, pkt)
  if (res !== 0) {
    throw new FlashReadError(`The reader refused the read at ${addr} (code ${String(res)}).`)
  }

  let got = 0
  while (got < len) {
    const packet = await loader.transport.read(BLOCK_TIMEOUT)
    if (!(packet instanceof Uint8Array) || packet.length === 0) {
      throw new FlashReadError(`The reader stopped sending after ${got} of ${len} bytes.`)
    }
    if (got + packet.length > len) {
      throw new FlashReadError(`The reader sent more than it was asked for at ${got} bytes.`)
    }
    // A block short of BLOCK before the end means bytes were lost on the way.
    // Our running total would then trail what the stub believes it sent, it
    // would wait for an acknowledgement that can never arrive, and so would
    // we. esptool.py refuses the same case rather than deadlock on it.
    if (packet.length < BLOCK && got + packet.length < len) {
      throw new FlashReadError(
        `The reader sent ${packet.length} bytes where a whole block was due, at ${got} bytes.`,
      )
    }
    out.set(packet, at + got)
    got += packet.length
    // The stub waits for a running total before it sends more.
    await loader.transport.write(loader._intToByteArray(got))
    onBlock(got)
  }

  const digest = await loader.transport.read(BLOCK_TIMEOUT)
  if (!(digest instanceof Uint8Array) || digest.length !== DIGEST_BYTES) {
    throw new FlashReadError(`The reader did not sign off the block at ${addr}.`)
  }
  const want = hex(digest)
  const mine = md5Hex(out.subarray(at, at + len))
  if (mine !== want) {
    throw new FlashReadError(
      `The block at ${addr} arrived corrupted: the reader signed it ${want} and it hashes to ${mine}.`,
    )
  }
}

/**
 * Puts the stub back where a fresh command can find it.
 *
 * A chunk that failed leaves the stub mid-errand: it is either still streaming
 * or waiting on a running total that will never reach the figure it expects.
 * Claiming the whole chunk lets it finish and sign off; then whatever it says
 * is read and dropped until the port falls quiet.
 */
async function resync(loader: FlashReader, len: number): Promise<void> {
  try {
    await loader.transport.write(loader._intToByteArray(len))
  } catch {
    // The port may already be past listening; the drain below still settles it.
  }
  const deadline = Date.now() + BLOCK_TIMEOUT
  while (Date.now() < deadline) {
    try {
      await loader.transport.read(QUIET_TIMEOUT)
    } catch {
      return // A read that times out is a port with nothing left to say.
    }
  }
}

/**
 * Read `size` bytes from `addr` into one preallocated buffer.
 *
 * `onProgress` is called with the running total, at most about ten times a
 * second, so repainting never competes with draining the serial port.
 *
 * Each chunk is checked against the digest the stub sends for it and read
 * again if anything about it was wrong, because over a browser serial port a
 * chunk does sometimes stall part way through. Retrying costs one chunk;
 * failing the read costs all sixteen megabytes.
 */
export async function readFlashInto(
  loader: FlashReader,
  addr: number,
  size: number,
  onProgress?: (done: number, total: number) => void,
): Promise<Uint8Array> {
  const out = new Uint8Array(size)
  let done = 0
  let lastTick = 0

  // `received` is the running total including the chunk in hand: a chunk is a
  // sixteenth of the read, so reporting only finished chunks pins the bar at 0%
  // for the whole first megabyte, which reads as a hang.
  const tick = (received: number, force: boolean): void => {
    if (!onProgress) return
    const now = Date.now()
    if (force || now - lastTick >= 100) {
      lastTick = now
      onProgress(received, size)
    }
  }

  while (done < size) {
    const len = Math.min(CHUNK, size - done)
    let lastError: unknown = null

    for (let attempt = 1; attempt <= CHUNK_ATTEMPTS; attempt++) {
      try {
        await readChunk(loader, addr + done, len, out, done, (got) => tick(done + got, false))
        lastError = null
        break
      } catch (err) {
        lastError = err
        // The bar has been showing this chunk's progress; wind it back so the
        // second attempt does not look like the first one going backwards.
        tick(done, true)
        if (attempt < CHUNK_ATTEMPTS) await resync(loader, len)
      }
    }

    if (lastError) {
      const why = lastError instanceof Error ? lastError.message : String(lastError)
      throw new FlashReadError(
        `The read stalled at ${done} bytes and did not recover after ${CHUNK_ATTEMPTS} attempts: ${why} ` +
          `Nothing has been saved. Try again, and prefer a direct adapter to a multi-port dongle.`,
      )
    }

    done += len
    tick(done, true)
  }

  return out
}
