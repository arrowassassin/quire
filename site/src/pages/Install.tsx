import { useCallback, useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import { Link } from 'react-router-dom'
import type { ESPLoader, Transport } from 'esptool-js'
import { Callout } from '../components/Callout'
import { DownloadIcon } from '../components/Icons'
import { ACTIONS_URL, FACTORY_IMAGE, FLASH_BYTES, RELEASES_URL } from '../data/site'
import {
  backupName,
  blobOf,
  bytes,
  imageProblem,
  megabytes,
  percent,
  roughly,
  saveFile,
  sha256Hex,
  tracker,
  type Progress,
} from '../lib/flash'
import { md5Hex } from '../lib/md5'
import { readFlashInto, SERIAL_BUFFER_BYTES } from '../lib/readflash'
import { probeLatestRelease, type ReleaseProbe } from '../lib/release'
import { useSeo } from '../lib/useSeo'

/* -------------------------------------------------------------------------
   Small pieces
   ------------------------------------------------------------------------- */

type StepState = 'locked' | 'ready' | 'done'

interface StepProps {
  n: number
  title: string
  state: StepState
  /** The backup step is the one that matters; it gets the visual weight. */
  feature?: boolean
  children: ReactNode
}

function Step({ n, title, state, feature, children }: StepProps) {
  return (
    <li className={feature ? 'stepx stepx--feature' : 'stepx'} data-state={state}>
      <div className="stepx__head">
        <span className="stepx__num" aria-hidden="true">
          {String(n).padStart(2, '0')}
        </span>
        <h2>
          <span className="visually-hidden">{`Step ${n}. `}</span>
          {title}
        </h2>
        <span className="stepx__flag" data-state={state}>
          {state === 'done' ? 'Done' : state === 'locked' ? 'Waiting' : 'Your turn'}
        </span>
      </div>
      <div className="stepx__body">{children}</div>
    </li>
  )
}

function Bar({ progress, label }: { progress: Progress; label: string }) {
  const pct = percent(progress)
  return (
    <div className="bar">
      <div
        className="bar__track"
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
        aria-valuetext={`${pct} per cent`}
      >
        <span className="bar__fill" style={{ width: `${pct}%` }} />
      </div>
      <p className="bar__meta">
        <span className="bar__pct">{pct}%</span>
        <span>
          {megabytes(progress.done)} of {megabytes(progress.total)}
        </span>
        <span>{progress.eta === null ? 'estimating…' : `${roughly(progress.eta)} left`}</span>
      </p>
    </div>
  )
}

/** Turns an exception into something a person can act on. */
function connectAdvice(err: unknown): string {
  const name = err instanceof DOMException ? err.name : ''
  const message = err instanceof Error ? err.message : String(err)

  if (name === 'NotFoundError') {
    return 'No device was chosen. The browser shows a list of serial ports; pick the one that appeared when you attached the cable, then press Connect again. If the list was empty, see the checklist below.'
  }
  if (name === 'SecurityError' || name === 'NotAllowedError') {
    return 'The browser refused access to the serial port. Choosing a port has to come from a click on this page, and the page has to be served over https — reload and press Connect again.'
  }
  if (name === 'InvalidStateError' || name === 'NetworkError' || /already open|failed to open/i.test(message)) {
    return 'The port could not be opened. Something else is probably holding it — a serial monitor, an Arduino IDE, another tab of this page. Close it and press Connect again.'
  }
  return `The reader did not answer (${message}). Work down the checklist below and press Connect again.`
}

/** Anything that goes wrong mid-operation, said plainly. */
function reason(err: unknown): string {
  if (err instanceof Error) return err.message
  return String(err)
}

const sleep = (ms: number) => new Promise((r) => window.setTimeout(r, ms))

interface DeviceInfo {
  chip: string
  mac: string
  flashSize: string
}

interface ChosenImage {
  name: string
  data: Uint8Array
  source: string
}

/* -------------------------------------------------------------------------
   The page
   ------------------------------------------------------------------------- */

export function Install() {
  useSeo({
    title: 'Install from your browser',
    description:
      'Install Quire on an Xteink X3 from Chrome or Edge: back up the stock firmware to a file, write the Quire image, and restore the backup later — no terminal and nothing to install on your computer.',
    path: '/install',
  })

  const [supported] = useState(() => typeof navigator !== 'undefined' && 'serial' in navigator)

  const transportRef = useRef<Transport | null>(null)
  const loaderRef = useRef<ESPLoader | null>(null)

  const [busy, setBusy] = useState<
    null | 'connect' | 'backup' | 'image' | 'install' | 'restore-file' | 'restore'
  >(null)

  const [device, setDevice] = useState<DeviceInfo | null>(null)
  const [connectError, setConnectError] = useState('')

  const [backupProgress, setBackupProgress] = useState<Progress | null>(null)
  const [backup, setBackup] = useState<{ file: string; sha: string | null } | null>(null)
  const [backupError, setBackupError] = useState('')
  /** Set when someone says they already hold a backup, so step 2 can be passed. */
  const [backupHeld, setBackupHeld] = useState(false)
  /**
   * Step 2 is satisfied either by a backup read here or by someone saying they
   * already have one. Everything downstream asks this, never `backup` itself,
   * so the two answers cannot drift apart.
   */
  const backupSettled = backup !== null || backupHeld

  const [release, setRelease] = useState<ReleaseProbe | null>(null)
  const [image, setImage] = useState<ChosenImage | null>(null)
  const [imageError, setImageError] = useState('')

  const [confirmed, setConfirmed] = useState(false)
  const [installProgress, setInstallProgress] = useState<Progress | null>(null)
  const [installed, setInstalled] = useState(false)
  const [installError, setInstallError] = useState('')

  const [restoreImage, setRestoreImage] = useState<ChosenImage | null>(null)
  const [restoreProgress, setRestoreProgress] = useState<Progress | null>(null)
  const [restored, setRestored] = useState(false)
  const [restoreError, setRestoreError] = useState('')

  /* Ask GitHub whether a release exists, so step 3 never offers a button that
     cannot work. Today the answer is "none". This waits until the backup is
     settled and step 3 is actually in play: visiting the page should not fire a
     request at GitHub, and a 404 should not land in everyone's console. */
  useEffect(() => {
    if (!supported || !backupSettled || release !== null) return
    const ac = new AbortController()
    let live = true
    probeLatestRelease(ac.signal)
      .then((probe) => {
        if (live) setRelease(probe)
      })
      .catch(() => {
        if (live) setRelease({ state: 'unknown', reason: 'The check could not be completed.' })
      })
    return () => {
      live = false
      ac.abort()
    }
  }, [supported, backupSettled, release])

  const drop = useCallback(async () => {
    const transport = transportRef.current
    transportRef.current = null
    loaderRef.current = null
    setDevice(null)
    if (transport) {
      try {
        await transport.disconnect()
      } catch {
        // The port may already be gone; there is nothing further to close.
      }
    }
  }, [])

  // Leaving the page must not leave the port held open.
  useEffect(() => {
    return () => {
      const transport = transportRef.current
      transportRef.current = null
      loaderRef.current = null
      if (transport) void transport.disconnect().catch(() => undefined)
    }
  }, [])

  const connect = useCallback(async (): Promise<boolean> => {
    if (loaderRef.current) return true
    setBusy('connect')
    setConnectError('')
    let transport: Transport | null = null
    try {
      const port = await navigator.serial.requestPort()
      // esptool-js is ~1 MB of JavaScript; only this click pays for it.
      const { ESPLoader, Transport: SerialTransport } = await import('esptool-js')
      // The second argument is Transport's `tracing` flag: it console.logs every
      // packet, which over a 16 MB read is tens of thousands of lines.
      transport = new SerialTransport(port, false)
      // Without serialOptions the port opens with Web Serial's 255-byte read
      // buffer, far too small for the window the stub reads into; see
      // SERIAL_BUFFER_BYTES. This is passed to every open the loader makes,
      // including the one it redoes after changing the baud rate.
      const loader = new ESPLoader({
        transport,
        baudrate: 921600,
        serialOptions: { bufferSize: SERIAL_BUFFER_BYTES },
      })
      const chip = await loader.main()
      const mac = await loader.chip.readMac(loader)
      const flashSize = await loader.detectFlashSize()
      transportRef.current = transport
      loaderRef.current = loader
      setDevice({ chip, mac, flashSize })
      return true
    } catch (err) {
      if (transport) {
        try {
          await transport.disconnect()
        } catch {
          // Already closed, or never opened.
        }
      }
      transportRef.current = null
      loaderRef.current = null
      setDevice(null)
      setConnectError(connectAdvice(err))
      return false
    } finally {
      setBusy(null)
    }
  }, [])

  const disconnect = useCallback(async () => {
    setBusy('connect')
    await drop()
    setBusy(null)
  }, [drop])

  const runBackup = useCallback(async () => {
    const loader = loaderRef.current
    if (!loader) return
    setBusy('backup')
    setBackupError('')
    setBackup(null)
    setBackupProgress({ done: 0, total: FLASH_BYTES, eta: null })
    const bump = tracker(setBackupProgress)
    try {
      const data = await readFlashInto(loader, 0, FLASH_BYTES, bump)

      if (data.length !== FLASH_BYTES) {
        // A short read is not a backup, and the transport is still healthy —
        // so this one keeps the connection and offers the read again.
        setBackupProgress(null)
        setBackupError(
          `The read came back ${bytes(data.length)} bytes. A backup is exactly ${bytes(
            FLASH_BYTES,
          )} bytes, so this one is incomplete and has not been saved. Read it again.`,
        )
        return
      }

      const name = backupName(new Date())
      const sha = await sha256Hex(data)
      saveFile(name, blobOf(data, 'application/octet-stream'))
      if (sha) {
        await sleep(600) // Two downloads in a row: give the first one its moment.
        saveFile(`${name}.sha256`, new Blob([`${sha}  ${name}\n`], { type: 'text/plain' }))
      }
      setBackupProgress(null)
      setBackupHeld(false)
      setBackup({ file: name, sha })
    } catch (err) {
      setBackupProgress(null)
      setBackupError(
        `The read stopped: ${reason(err)}. Nothing was written to the reader. The cable has been released — power the X3 on, attach the cable again, connect, and read it again.`,
      )
      await drop()
    } finally {
      setBusy(null)
    }
  }, [drop])

  const takeFile = useCallback(
    async (file: File, into: 'image' | 'restore') => {
      setBusy(into === 'image' ? 'image' : 'restore-file')
      if (into === 'image') {
        setImageError('')
        setImage(null)
      } else {
        setRestoreError('')
        setRestoreImage(null)
        setRestored(false)
      }
      try {
        const data = new Uint8Array(await file.arrayBuffer())
        const problem = imageProblem(data, file.name)
        if (problem) {
          if (into === 'image') setImageError(problem)
          else setRestoreError(problem)
          return
        }
        const chosen: ChosenImage = { name: file.name, data, source: 'from your computer' }
        if (into === 'image') setImage(chosen)
        else setRestoreImage(chosen)
      } catch (err) {
        const message = `That file could not be read: ${reason(err)}`
        if (into === 'image') setImageError(message)
        else setRestoreError(message)
      } finally {
        setBusy(null)
      }
    },
    [],
  )


  const write = useCallback(
    async (what: ChosenImage, kind: 'install' | 'restore') => {
      const loader = loaderRef.current
      if (!loader) return
      setBusy(kind)
      if (kind === 'install') setInstallError('')
      else setRestoreError('')
      const setProgress = kind === 'install' ? setInstallProgress : setRestoreProgress
      setProgress({ done: 0, total: what.data.length, eta: null })
      const bump = tracker(setProgress)
      try {
        await loader.writeFlash({
          fileArray: [{ data: what.data, address: 0 }],
          flashMode: 'keep',
          flashFreq: 'keep',
          flashSize: 'keep',
          eraseAll: false,
          // The padding in a 16 MB image deflates to almost nothing, so this is
          // the difference between a couple of minutes and a great many.
          compress: true,
          // Given this, writeFlash asks the chip for the MD5 of what actually
          // landed and throws if it differs, so a bad write is caught here
          // rather than by a reader that will not start.
          calculateMD5Hash: (image: Uint8Array) => md5Hex(image),
          reportProgress: (_fileIndex, written, total) => bump(written, total),
        })
        await loader.after('hard_reset')
        if (kind === 'install') setInstalled(true)
        else setRestored(true)
      } catch (err) {
        const message = `The write stopped: ${reason(err)}. The reader is very likely half-written, which is not fatal: power it on, attach the cable, connect again and write the same file again. The cable has been released.`
        if (kind === 'install') setInstallError(message)
        else setRestoreError(message)
        await drop()
      } finally {
        setProgress(null)
        setBusy(null)
      }
    },
    [drop],
  )

  /* ---------------------------------------------------------------------- */

  const working = busy !== null
  const connectState: StepState = device ? 'done' : 'ready'
  const backupState: StepState = !device ? 'locked' : backupSettled ? 'done' : 'ready'
  const imageState: StepState = !backupSettled ? 'locked' : image ? 'done' : 'ready'
  const installState: StepState = !image ? 'locked' : installed ? 'done' : 'ready'

  return (
    <>
      <header className="page-head">
        <div className="wrap wrap--narrow">
          <span className="eyebrow">Install</span>
          <h1>Install Quire from your browser.</h1>
          <p>
            Four steps on this page: connect the reader, save a copy of the firmware it came
            with, choose the Quire image, write it. Nothing to install on your computer, and no
            terminal.
          </p>
        </div>
      </header>

      <div className="wrap install">
        <Callout title="What this page is, plainly">
          <p>
            This project has no physical X3 to test against, so the installer below has been
            written against the chip’s documented protocol rather than proven on a device.{' '}
            <strong>Step 2 only reads</strong> — it cannot change anything on your reader — and
            nothing is written until you click a button that says it will write. If anything
            misbehaves, the{' '}
            <Link to="/guide#terminal">command-line route in the guide</Link> does the same job
            with the same numbers.
          </p>
        </Callout>

        {supported ? (
          <ol className="steps">
            {/* 1 ------------------------------------------------------- */}
            <Step n={1} title="Connect the reader" state={connectState}>
              <p>
                Three things decide whether this works at all, and all of them come before the
                button:
              </p>
              <ul className="ticks">
                <li>
                  <strong>Power the X3 on first, then attach the pogo cable.</strong> A reader
                  that is off will not appear in the browser’s list of ports.
                </li>
                <li>
                  <strong>The cable ends in USB-A.</strong> A computer with only USB-C ports
                  needs an adapter. A plain passive USB-C to USB-A adapter is more reliable here
                  than a multi-port dongle, some of which pass USB 2.0 devices poorly or carry
                  power only.
                </li>
                <li>
                  Some X3 units left the factory with the chip’s <em>download mode</em> fuse
                  burned. Those readers never appear over the cable, in any tool, and it cannot
                  be undone. If nothing shows up on any cable or port, that is most likely why.
                </li>
              </ul>

              <p className="dim">
                The browser will ask which port to use. The reader is the one named{' '}
                <code>usbmodem</code> followed by digits on macOS, <code>ttyACM</code> on Linux,
                or listed as a COM port on Windows. Bluetooth entries and anything named{' '}
                <code>debug-console</code> are the computer’s own devices, not the reader: if
                there is no <code>usbmodem</code> or <code>ttyACM</code> in the list, the reader
                is not reaching the computer at all, and no choice here will help.
              </p>

              <div className="actions">
                <button
                  type="button"
                  className="btn"
                  onClick={() => void connect()}
                  disabled={working || device !== null}
                >
                  {busy === 'connect' ? 'Connecting…' : device ? 'Connected' : 'Connect the reader'}
                </button>
                {device ? (
                  <button
                    type="button"
                    className="btn btn--ghost"
                    onClick={() => void disconnect()}
                    disabled={working}
                  >
                    Disconnect
                  </button>
                ) : null}
              </div>

              <p className="status" aria-live="polite">
                {device
                  ? `Connected. ${device.chip}, MAC ${device.mac}, ${device.flashSize} of flash.`
                  : busy === 'connect'
                    ? 'Asking the browser for a serial port…'
                    : 'Not connected yet.'}
              </p>

              {device ? (
                <dl className="kv">
                  <div>
                    <dt>Chip</dt>
                    <dd>{device.chip}</dd>
                  </div>
                  <div>
                    <dt>MAC</dt>
                    <dd>{device.mac}</dd>
                  </div>
                  <div>
                    <dt>Flash</dt>
                    <dd>{device.flashSize}</dd>
                  </div>
                </dl>
              ) : null}

              {connectError ? (
                <div className="note note--bad" role="alert">
                  <p>{connectError}</p>
                  <ul>
                    <li>Is the reader switched on? Turn it on, then attach the cable.</li>
                    <li>
                      Are the pogo pins seated squarely? If the cable runs through a multi-port
                      dongle, try a plain USB-C to USB-A adapter or a port on the computer
                      itself.
                    </li>
                    <li>Is another program holding the port — a serial monitor, an IDE, a second tab of this page?</li>
                    <li>
                      If no port ever appears, the unit may be one of the flash-locked ones.{' '}
                      <Link to="/guide#locked">What that means</Link>.
                    </li>
                  </ul>
                </div>
              ) : null}
            </Step>

            {/* 2 ------------------------------------------------------- */}
            <Step n={2} title="Back up the firmware your reader came with" state={backupState} feature>
              <p className="lead">
                The factory firmware is not published anywhere, and this project cannot give it
                back to you. The copy you make here is the only way back to the reader you
                bought.
              </p>
              <p>
                This reads all 16 MB of the flash into a file on your computer. It writes
                nothing, and it takes a few minutes. Leave the cable alone while it runs.
              </p>

              <div className="actions">
                <button
                  type="button"
                  className="btn"
                  onClick={() => void runBackup()}
                  disabled={working || !device}
                >
                  {busy === 'backup'
                    ? 'Reading…'
                    : backup
                      ? 'Read it again'
                      : backupError
                        ? 'Try the backup again'
                        : 'Back up the stock firmware'}
                </button>
                {/* Someone on their second reader, or coming back to a half-finished
                    install, already has the only file this step can produce. Reading
                    it again costs them five minutes and tells them nothing new. */}
                {!backupSettled ? (
                  <button
                    type="button"
                    className="btn btn--ghost"
                    onClick={() => setBackupHeld(true)}
                    disabled={working || !device}
                  >
                    I already have one
                  </button>
                ) : null}
              </div>

              {backupProgress ? <Bar progress={backupProgress} label="Reading the flash" /> : null}

              <p className="status" aria-live="polite">
                {backup
                  ? `Saved ${backup.file} — ${bytes(FLASH_BYTES)} bytes, the exact size a whole-flash image has to be.`
                  : backupHeld
                    ? 'Taken as read: you have a backup already.'
                    : busy === 'backup'
                    ? 'Reading the flash. Do not detach the cable.'
                    : device
                      ? 'Nothing read yet.'
                      : 'Connect the reader first.'}
              </p>

              {backup ? (
                <div className="note note--good">
                  <p>
                    Two files have been handed to your browser:{' '}
                    <code>{backup.file}</code>
                    {backup.sha ? (
                      <>
                        {' '}
                        and <code>{backup.file}.sha256</code>
                      </>
                    ) : null}
                    . They are in whatever folder your browser saves downloads to.
                  </p>
                  {backup.sha ? (
                    <p className="hash">
                      SHA-256 <code>{backup.sha}</code>
                    </p>
                  ) : (
                    <p>
                      The checksum could not be computed in this browser, so only the{' '}
                      <code>.bin</code> was saved. That is still a complete backup.
                    </p>
                  )}
                  <p>
                    <strong>
                      Copy both somewhere that is not this computer
                    </strong>{' '}
                    — a second disk, a USB stick, a cloud folder. A backup that lives only on the
                    machine you are about to experiment with is half a backup.
                  </p>
                </div>
              ) : null}

              {backupHeld && !backup ? (
                <div className="note">
                  <p>
                    Nothing has been read, so nothing here has been checked. Before you go on,
                    make sure the file you are relying on is {bytes(FLASH_BYTES)} bytes and is
                    somewhere other than this computer. <a href="#restore-title">Putting the stock
                    firmware back</a> will ask for it.
                  </p>
                  <p>
                    <button
                      type="button"
                      className="btn btn--ghost"
                      onClick={() => setBackupHeld(false)}
                    >
                      Actually, read it now
                    </button>
                  </p>
                </div>
              ) : null}

              {backupError ? (
                <div className="note note--bad" role="alert">
                  <p>{backupError}</p>
                </div>
              ) : null}
            </Step>

            {/* 3 ------------------------------------------------------- */}
            <Step n={3} title="Choose the Quire image" state={imageState}>
              <p>
                The file to write is <code>{FACTORY_IMAGE}</code>: the complete 16 MB flash
                image — bootloader, partition table, recovery app, firmware and dictionary.
                Whatever you choose is checked before anything happens to your reader.
              </p>

              <div className="pick">
                <label className="pick__file">
                  <span className="pick__label">A file on your computer</span>
                  <input
                    type="file"
                    accept=".bin,application/octet-stream"
                    disabled={working || !backupSettled}
                    onChange={(e) => {
                      const file = e.target.files?.[0]
                      e.target.value = ''
                      if (file) void takeFile(file, 'image')
                    }}
                  />
                </label>

                <div className="pick__release">
                  <span className="pick__label">Or straight from a release</span>
                  {!backupSettled ? (
                    <p className="muted">Checked once step 2 is settled.</p>
                  ) : release === null ? (
                    <p className="muted">Checking whether a release has been published…</p>
                  ) : release.state === 'available' ? (
                    <>
                      {/* A plain download, not a fetch. GitHub serves release assets
                          from release-assets.githubusercontent.com, which sends no
                          access-control-allow-origin, so no page may read one however
                          it asks. An ordinary download is not subject to that, and the
                          file picker above takes it from there. */}
                      <a
                        className="btn btn--ghost"
                        href={release.url}
                        download={release.asset}
                        rel="noreferrer"
                      >
                        <DownloadIcon />
                        {`Download ${release.asset} (${release.tag})`}
                      </a>
                      <p className="muted">
                        {megabytes(release.size)}. It goes to your downloads folder; then choose
                        it with the file picker.
                      </p>
                    </>
                  ) : (
                    <p className="muted">
                      {release.state === 'none'
                        ? 'No release has been published yet, so there is nothing here to download.'
                        : release.state === 'no-asset'
                          ? `The latest release${release.tag ? ` (${release.tag})` : ''} does not carry ${FACTORY_IMAGE}.`
                          : `${release.reason} You can still use the file picker.`}
                    </p>
                  )}
                </div>
              </div>

              {backupSettled && release !== null && release.state !== 'available' ? (
                <div className="note">
                  <p>
                    Until a version is tagged, the newest build is the{' '}
                    <code>quire-x3-images</code> artifact attached to the latest successful CI
                    run. Download it, unzip it, and pick <code>{FACTORY_IMAGE}</code> with the
                    file picker above.
                  </p>
                  <p className="actions">
                    <a className="btn btn--ghost btn--sm" href={ACTIONS_URL} target="_blank" rel="noreferrer">
                      Latest CI images
                    </a>
                    <a className="btn btn--ghost btn--sm" href={RELEASES_URL} target="_blank" rel="noreferrer">
                      Releases
                    </a>
                    <Link className="btn btn--ghost btn--sm" to="/downloads">
                      What is in the artifact
                    </Link>
                  </p>
                </div>
              ) : null}

              <p className="status" aria-live="polite">
                {image
                  ? `${image.name} is ready — ${bytes(image.data.length)} bytes, ${image.source}.`
                  : busy === 'image'
                    ? 'Reading the file…'
                    : backupSettled
                      ? 'No image chosen yet.'
                      : 'Take the backup first.'}
              </p>

              {imageError ? (
                <div className="note note--bad" role="alert">
                  <p>{imageError}</p>
                </div>
              ) : null}
            </Step>

            {/* 4 ------------------------------------------------------- */}
            <Step n={4} title="Write Quire to the reader" state={installState}>
              <p>
                This replaces everything on the reader’s flash with{' '}
                <code>{image ? image.name : FACTORY_IMAGE}</code>, then restarts it. It takes a
                couple of minutes. Do not detach the cable while it runs.
              </p>

              <label className="confirm">
                <input
                  type="checkbox"
                  checked={confirmed}
                  disabled={working || !image || installed}
                  onChange={(e) => setConfirmed(e.target.checked)}
                />
                <span>
                  {backup
                    ? 'My backup from step 2 is saved, and I have a copy of it somewhere other than this computer.'
                    : 'I have a whole-flash backup of this reader, and a copy of it somewhere other than this computer.'}
                </span>
              </label>

              <div className="actions">
                <button
                  type="button"
                  className="btn"
                  onClick={() => image && void write(image, 'install')}
                  disabled={working || !image || !confirmed || installed}
                >
                  {busy === 'install' ? 'Writing…' : 'Write Quire at 0x0 now'}
                </button>
              </div>

              {installProgress ? <Bar progress={installProgress} label="Writing the image" /> : null}

              <p className="status" aria-live="polite">
                {installed
                  ? 'Written, and the reader has been restarted.'
                  : busy === 'install'
                    ? 'Writing. Do not detach the cable.'
                    : image
                      ? 'Nothing has been written yet.'
                      : 'Choose an image first.'}
              </p>

              {installed ? (
                <div className="note note--good">
                  <p>
                    The reader should wake into Quire’s first-run questions: the time, the
                    hyphenation language, the reading defaults and where books live on the card.
                    All four are in <em>Settings</em> afterwards, so none of the answers are
                    permanent.
                  </p>
                  <p>
                    You can detach the cable now. Put a FAT32 microSD card in with your books in{' '}
                    <code>/Books</code>.
                  </p>
                  <p>
                    <Link className="link-arrow" to="/guide#firstrun">
                      What the first run asks, step by step
                    </Link>
                  </p>
                </div>
              ) : null}

              {installError ? (
                <div className="note note--bad" role="alert">
                  <p>{installError}</p>
                </div>
              ) : null}
            </Step>
          </ol>
        ) : (
          <section className="unsupported" aria-labelledby="unsupported-title">
            <span className="eyebrow">Not in this browser</span>
            <h2 id="unsupported-title">This browser cannot talk to the reader.</h2>
            <p className="lead">
              Writing firmware over the cable needs the Web Serial API, and this browser does not
              have it. Nothing is wrong with your reader or your computer.
            </p>
            <p>You need:</p>
            <ul className="ticks">
              <li>
                <strong>Chrome, Edge, Opera, Brave or another Chromium-based browser.</strong>{' '}
                Firefox and Safari have not implemented Web Serial.
              </li>
              <li>
                <strong>A desktop or laptop computer</strong> — Windows, macOS, Linux or
                ChromeOS. Phones and tablets cannot do this, including Chrome on Android and
                anything on iPhone or iPad.
              </li>
            </ul>
            <p>
              If you would rather not change browser, the terminal route does exactly the same
              thing, with the same addresses and the same 16 MB backup.
            </p>
            <p className="actions">
              <Link className="btn" to="/guide#terminal">
                Install from a terminal instead
              </Link>
              <Link className="btn btn--ghost" to="/guide">
                The whole guide
              </Link>
            </p>
          </section>
        )}

        {/* Restore --------------------------------------------------- */}
        <section className="restore" id="restore" aria-labelledby="restore-title">
          <span className="eyebrow">Any time</span>
          <h2 id="restore-title">Put the stock firmware back</h2>
          <p className="lead">
            This is the promise the backup step makes. Choose the <code>.bin</code> you saved and
            it goes back where it came from, at address 0.
          </p>

          {supported ? (
            <>
              <div className="pick">
                <label className="pick__file">
                  <span className="pick__label">Your backup file</span>
                  <input
                    type="file"
                    accept=".bin,application/octet-stream"
                    disabled={working}
                    onChange={(e) => {
                      const file = e.target.files?.[0]
                      e.target.value = ''
                      if (file) void takeFile(file, 'restore')
                    }}
                  />
                </label>
              </div>

              <div className="actions">
                {device ? null : (
                  <button
                    type="button"
                    className="btn btn--ghost"
                    onClick={() => void connect()}
                    disabled={working}
                  >
                    {busy === 'connect' ? 'Connecting…' : 'Connect the reader'}
                  </button>
                )}
                <button
                  type="button"
                  className="btn"
                  onClick={() => restoreImage && void write(restoreImage, 'restore')}
                  disabled={working || !device || !restoreImage}
                >
                  {busy === 'restore' ? 'Writing…' : 'Write this backup at 0x0 now'}
                </button>
              </div>

              {restoreProgress ? (
                <Bar progress={restoreProgress} label="Writing the backup back" />
              ) : null}

              <p className="status" aria-live="polite">
                {restored
                  ? 'The backup has been written and the reader restarted. It is the reader you bought again.'
                  : busy === 'restore'
                    ? 'Writing. Do not detach the cable.'
                    : restoreImage
                      ? `${restoreImage.name} is ready — ${bytes(restoreImage.data.length)} bytes.${
                          device ? '' : ' Connect the reader to write it.'
                        }`
                      : 'No backup file chosen yet.'}
              </p>

              {restoreError ? (
                <div className="note note--bad" role="alert">
                  <p>{restoreError}</p>
                </div>
              ) : null}
            </>
          ) : (
            <p>
              Restoring needs the same Web Serial support as installing. In a Chromium browser
              this section takes your backup file and writes it back; from a terminal it is one{' '}
              <code>espflash write-bin</code> command, in{' '}
              <Link to="/guide#terminal">the guide</Link>.
            </p>
          )}
        </section>

        <p className="install__tail">
          <Link className="link-arrow" to="/guide">
            The rest of the guide: books, the Drop page, updates and recovery
          </Link>
        </p>
      </div>
    </>
  )
}
