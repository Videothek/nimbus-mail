<script lang="ts" module>
  // Cache: video bytes → first-frame data URL.  Keyed by the
  // bytes reference (Compose attaches reuse the same number[]
  // across renders) plus an optional explicit string key for
  // hosts that build fresh byte arrays each time (MailView).
  // Decoding once costs a transient GStreamer pipeline init
  // on Linux WebKit; subsequent mounts of the same file render
  // a plain <img> with no codec activity at all.
  const FRAME_CACHE = new Map<unknown, string>()
  // Serialise extractions so a folder full of videos doesn't
  // spin up N pipelines simultaneously — Linux WebKit hits a
  // visible stall when more than two or three pipelines are
  // alive at once.
  let extractionChain: Promise<void> = Promise.resolve()

  function extractFirstFrame(blobUrl: string): Promise<string | null> {
    return new Promise<string | null>((resolve) => {
      const v = document.createElement('video')
      v.muted = true
      v.playsInline = true
      v.preload = 'metadata'
      v.src = blobUrl
      let resolved = false
      const finish = (val: string | null) => {
        if (resolved) return
        resolved = true
        try {
          v.removeAttribute('src')
          v.load()
        } catch {
          /* swallow — element is being torn down anyway */
        }
        v.remove()
        resolve(val)
      }
      const draw = () => {
        try {
          const canvas = document.createElement('canvas')
          canvas.width = v.videoWidth || 192
          canvas.height = v.videoHeight || 192
          const ctx = canvas.getContext('2d')
          if (!ctx) return finish(null)
          ctx.drawImage(v, 0, 0, canvas.width, canvas.height)
          finish(canvas.toDataURL('image/png'))
        } catch {
          finish(null)
        }
      }
      v.addEventListener('loadeddata', () => {
        try {
          v.currentTime = Math.min(0.1, v.duration || 0.1)
        } catch {
          draw()
        }
      })
      v.addEventListener('seeked', draw)
      v.addEventListener('error', () => finish(null))
      // Hard cap so a stuck decoder doesn't pin the chain.
      setTimeout(() => finish(null), 8000)
    })
  }
</script>

<script lang="ts">
  // Square inline thumbnail for an attachment.  Image: blob URL
  // → <img> directly.  Video: first frame extracted to a data
  // URL once, cached, rendered as <img> — no <video> elements
  // in the live DOM, which keeps the GStreamer cost limited to
  // the one-time extraction.  Everything else falls through to
  // <FileTypeIcon>.
  import FileTypeIcon from './FileTypeIcon.svelte'

  interface Props {
    /** Bytes the host already has in memory (Compose path).
     *  Mutually exclusive with `bytesProvider`. */
    bytes?: Uint8Array | number[] | null
    /** Async loader for cases where bytes aren't in memory yet
     *  (MailView lazy-fetches via download_email_attachment).
     *  Resolves to the raw bytes, or `null` to fall back to the
     *  typed icon. */
    bytesProvider?: () => Promise<Uint8Array | number[] | null>
    contentType?: string | null
    filename: string
    /** Tailwind sizing — default w-9 h-9. */
    class?: string
    /** Stable key for the video first-frame cache.  When the
     *  host always passes the same `bytes` reference (Compose),
     *  omit it and the bytes ref doubles as the key.  When the
     *  host produces fresh byte arrays each mount (MailView's
     *  `bytesProvider`), pass an account/path-derived string so
     *  re-mounts hit the cache instead of re-decoding. */
    cacheKey?: string | null
  }
  let {
    bytes = null,
    bytesProvider,
    contentType = '',
    filename,
    class: cls = 'w-9 h-9',
    cacheKey = null,
  }: Props = $props()

  function ext(): string {
    const dot = filename.lastIndexOf('.')
    return dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
  }
  function isImage(): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('image/')) return true
    return ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'tif', 'tiff', 'heic', 'heif', 'svg'].includes(ext())
  }
  function isVideo(): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('video/')) return true
    return ['mp4', 'mkv', 'mov', 'avi', 'webm', 'm4v', 'mpg', 'mpeg', '3gp', 'wmv', 'flv'].includes(ext())
  }

  /** Source for the rendered <img>.  For images: a blob URL.
   *  For videos: a data URL produced by canvas first-frame
   *  extraction (cached so subsequent mounts skip the decode). */
  let imgUrl = $state<string | null>(null)

  function makeBlobUrl(b: Uint8Array | number[]): string {
    let ct = contentType ?? ''
    if (!ct) ct = isVideo() ? 'video/mp4' : 'image/png'
    const u8 = b instanceof Uint8Array ? b : new Uint8Array(b)
    return URL.createObjectURL(new Blob([u8], { type: ct }))
  }

  function frameKey(b: Uint8Array | number[]): unknown {
    return cacheKey ?? b
  }

  async function loadFrame(b: Uint8Array | number[]): Promise<string | null> {
    const key = frameKey(b)
    const hit = FRAME_CACHE.get(key)
    if (hit) return hit
    const url = makeBlobUrl(b)
    // Chain through the global queue so concurrent mounts don't
    // each instantiate a GStreamer pipeline at the same time.
    let frame: string | null = null
    extractionChain = extractionChain.then(async () => {
      frame = await extractFirstFrame(url)
    })
    await extractionChain
    URL.revokeObjectURL(url)
    if (frame) FRAME_CACHE.set(key, frame)
    return frame
  }

  $effect(() => {
    if (!isImage() && !isVideo()) {
      imgUrl = null
      return
    }
    let cancelled = false
    let createdBlobUrl: string | null = null
    const apply = async (b: Uint8Array | number[] | null | undefined) => {
      if (!b || b.length === 0 || cancelled) return
      if (isImage()) {
        createdBlobUrl = makeBlobUrl(b)
        imgUrl = createdBlobUrl
      } else if (isVideo()) {
        const frame = await loadFrame(b)
        if (cancelled) return
        imgUrl = frame
      }
    }
    if (bytes && bytes.length > 0) {
      void apply(bytes)
    } else if (bytesProvider) {
      void (async () => {
        try {
          const b = await bytesProvider()
          if (cancelled) return
          await apply(b)
        } catch (e) {
          console.warn('attachment thumb load failed', e)
        }
      })()
    } else {
      imgUrl = null
    }
    return () => {
      cancelled = true
      if (createdBlobUrl) URL.revokeObjectURL(createdBlobUrl)
    }
  })
</script>

{#if imgUrl}
  <img
    src={imgUrl}
    alt=""
    loading="lazy"
    class="{cls} object-cover rounded shrink-0 bg-surface-200 dark:bg-surface-800"
  />
{:else}
  <!-- Fallback while a video frame is extracting (or for non-
       previewable types).  Replaced live by an <img> once the
       extraction chain resolves; cached extractions skip the
       icon and render the image immediately. -->
  <FileTypeIcon contentType={contentType} filename={filename} class={cls} />
{/if}
