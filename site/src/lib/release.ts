/**
 * Looking up the factory image published with a GitHub release.
 *
 * Only the lookup happens here. The asset itself is not fetched: GitHub serves
 * release assets from release-assets.githubusercontent.com, which answers
 * without an access-control-allow-origin header, so a page cannot read one
 * whichever URL it asks for — the API asset URL redirects to the same place.
 * The page therefore offers an ordinary download link, which CORS does not
 * govern, and takes the file back through the picker.
 *
 * The page asks first and only offers the link when there is really something
 * behind it — no dead or invented download.
 */
import { FACTORY_IMAGE, RELEASES_API } from '../data/site'

export type ReleaseProbe =
  | { state: 'available'; tag: string; asset: string; url: string; size: number }
  | { state: 'none' }
  | { state: 'no-asset'; tag: string }
  | { state: 'unknown'; reason: string }

interface Asset {
  name: string
  url: string
  size: number
}

function readRelease(body: unknown): { tag: string; assets: Asset[] } {
  if (typeof body !== 'object' || body === null) return { tag: '', assets: [] }
  const record = body as Record<string, unknown>
  const tag = typeof record['tag_name'] === 'string' ? record['tag_name'] : ''
  const raw = Array.isArray(record['assets']) ? record['assets'] : []
  const assets: Asset[] = []
  for (const entry of raw) {
    if (typeof entry !== 'object' || entry === null) continue
    const a = entry as Record<string, unknown>
    const name = a['name']
    const url = a['browser_download_url']
    const size = a['size']
    if (typeof name === 'string' && typeof url === 'string') {
      assets.push({ name, url, size: typeof size === 'number' ? size : 0 })
    }
  }
  return { tag, assets }
}

/** Asks GitHub whether a release carrying the factory image exists. */
export async function probeLatestRelease(signal?: AbortSignal): Promise<ReleaseProbe> {
  let res: Response
  try {
    res = await fetch(RELEASES_API, {
      headers: { Accept: 'application/vnd.github+json' },
      ...(signal ? { signal } : {}),
    })
  } catch {
    return { state: 'unknown', reason: 'GitHub could not be reached from this browser.' }
  }

  if (res.status === 404) return { state: 'none' }
  if (!res.ok) {
    return { state: 'unknown', reason: `GitHub answered ${res.status} when asked for the latest release.` }
  }

  let body: unknown
  try {
    body = await res.json()
  } catch {
    return { state: 'unknown', reason: 'GitHub’s answer could not be read as a release.' }
  }

  const { tag, assets } = readRelease(body)
  const asset = assets.find((a) => a.name === FACTORY_IMAGE)
  if (!asset) return { state: 'no-asset', tag }
  return { state: 'available', tag, asset: asset.name, url: asset.url, size: asset.size }
}
