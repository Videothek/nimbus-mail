<script lang="ts" module>
  // Cache: video bytes → first-frame data URL.  Two backings so
  // we can be honest about lifetimes:
  //
  // - WeakMap keyed by the bytes reference — used by hosts that
  //   reuse the same array each render (Compose).  When the
  //   Compose surface unmounts and drops the Attachment object,
  //   the bytes array becomes unreferenced and GC reclaims its
  //   cache entry too.  A "send" or "cancel" therefore frees
  //   every cached poster for that draft automatically.
  // - Map keyed by an explicit string id — used when the host
  //   produces fresh byte arrays per mount (MailView fetches
  //   bytes on demand).  Session-scoped so re-opening the same
  //   mail later doesn't re-decode.
  const FRAME_CACHE_REF = new WeakMap<object, string>()
  const FRAME_CACHE_KEY = new Map<string, string>()
  function cacheGet(key: unknown): string | undefined {
    if (typeof key === 'string') return FRAME_CACHE_KEY.get(key)
    if (key && typeof key === 'object') return FRAME_CACHE_REF.get(key as object)
    return undefined
  }
  function cachePut(key: unknown, val: string): void {
    if (typeof key === 'string') FRAME_CACHE_KEY.set(key, val)
    else if (key && typeof key === 'object') FRAME_CACHE_REF.set(key as object, val)
  }

  // Image blob URLs cached so re-mounts (e.g. opening the `/`
  // picker repeatedly) reuse the same Blob URL instead of
  // building a fresh one each time — building a Blob from a
  // multi-MB number[] is an O(n) copy that adds up across a
  // dropdown of attachments.
  const IMAGE_BLOB_REF = new WeakMap<object, string>()
  const IMAGE_BLOB_KEY = new Map<string, string>()
  function imageBlobGet(key: unknown): string | undefined {
    if (typeof key === 'string') return IMAGE_BLOB_KEY.get(key)
    if (key && typeof key === 'object') return IMAGE_BLOB_REF.get(key as object)
    return undefined
  }
  function imageBlobPut(key: unknown, val: string): void {
    if (typeof key === 'string') IMAGE_BLOB_KEY.set(key, val)
    else if (key && typeof key === 'object') IMAGE_BLOB_REF.set(key as object, val)
  }

  function isImageGuess(contentType: string | null | undefined, filename: string): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('image/')) return true
    const dot = filename.lastIndexOf('.')
    const ext = dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
    return ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'tif', 'tiff', 'heic', 'heif', 'svg'].includes(ext)
  }
  function isVideoGuess(contentType: string | null | undefined, filename: string): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('video/')) return true
    const dot = filename.lastIndexOf('.')
    const ext = dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
    return ['mp4', 'mkv', 'mov', 'avi', 'webm', 'm4v', 'mpg', 'mpeg', '3gp', 'wmv', 'flv'].includes(ext)
  }

  /** Pre-warm the image blob URL + video first-frame caches for
   *  an attachment.  Compose calls this the moment an attachment
   *  is added so the editor's `/` picker, which may open
   *  milliseconds later, sees fully-resolved thumbs without
   *  having to mount any preview component.
   *
   *  Image blob URLs are built synchronously (cheap O(n) array
   *  copy) so they're guaranteed available the instant the
   *  picker opens.  Video first-frame extraction is async (a
   *  GStreamer pipeline cycle on Linux WebKit) and runs through
   *  the global extractionChain so the picker may briefly show
   *  the typed icon for a video that's still decoding. */
  export function prewarm(opts: {
    bytes: Uint8Array | number[]
    contentType?: string | null
    filename: string
    cacheKey?: string | null
  }): void {
    const { bytes, contentType = null, filename, cacheKey = null } = opts
    if (!bytes || bytes.length === 0) return
    const key = cacheKey ?? bytes
    if (isImageGuess(contentType, filename)) {
      if (imageBlobGet(key)) return
      let ct = contentType ?? ''
      if (!ct) ct = 'image/png'
      const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
      const url = URL.createObjectURL(new Blob([u8], { type: ct }))
      imageBlobPut(key, url)
    } else if (isVideoGuess(contentType, filename)) {
      if (cacheGet(key)) return
      let ct = contentType ?? ''
      if (!ct) ct = 'video/mp4'
      const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
      const url = URL.createObjectURL(new Blob([u8], { type: ct }))
      extractionChain = extractionChain
        .then(async () => {
          const frame = await extractFirstFrame(url)
          if (frame) cachePut(key, frame)
        })
        .finally(() => URL.revokeObjectURL(url))
    }
  }

  /** Synchronous read of whatever's currently cached for a
   *  given attachment.  Used by hosts that want to render the
   *  thumb inline without paying the cost of mounting an
   *  AttachmentThumb component (the editor's `/` picker
   *  dropdown).  Returns the blob/data URL, or null if not
   *  cached yet — caller falls back to a typed icon. */
  export function thumbUrlSync(opts: {
    bytes?: Uint8Array | number[] | null
    contentType?: string | null
    filename: string
    cacheKey?: string | null
  }): string | null {
    const { bytes = null, cacheKey = null } = opts
    const key = cacheKey ?? bytes ?? null
    if (key === null) return null
    return imageBlobGet(key) ?? cacheGet(key) ?? null
  }
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
          // Scale the canvas to a thumbnail-sized output rather
          // than drawing at the source video's full resolution.
          // A 4K clip would otherwise produce a 3840×2160 PNG
          // encode per file — hundreds of ms each, serialised
          // through the extractionChain, which is exactly what
          // made the picker feel slow with video attachments.
          // 192 px on the long edge fits the largest preview
          // we render (NC picker) with room for high-DPI.
          const MAX_DIM = 192
          const sw = v.videoWidth || MAX_DIM
          const sh = v.videoHeight || MAX_DIM
          const scale = Math.min(1, MAX_DIM / Math.max(sw, sh))
          const canvas = document.createElement('canvas')
          canvas.width = Math.max(1, Math.round(sw * scale))
          canvas.height = Math.max(1, Math.round(sh * scale))
          const ctx = canvas.getContext('2d')
          if (!ctx) return finish(null)
          ctx.drawImage(v, 0, 0, canvas.width, canvas.height)
          // JPEG rather than PNG — ~10× smaller, a fraction of
          // the encode cost, and we don't need transparency for
          // a video frame.
          finish(canvas.toDataURL('image/jpeg', 0.78))
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
    const hit = cacheGet(key)
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
    if (frame) cachePut(key, frame)
    return frame
  }

  $effect(() => {
    if (!isImage() && !isVideo()) {
      imgUrl = null
      return
    }
    let cancelled = false
    const apply = async (b: Uint8Array | number[] | null | undefined) => {
      if (!b || b.length === 0 || cancelled) return
      if (isImage()) {
        const key = cacheKey ?? b
        const cached = imageBlobGet(key)
        if (cached) {
          imgUrl = cached
          return
        }
        const url = makeBlobUrl(b)
        imageBlobPut(key, url)
        imgUrl = url
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
      // Image blob URLs live in the cache and are reused across
      // mounts; we don't revoke them here.  When the bytes ref
      // becomes unreferenced (Compose unmounts and drops the
      // Attachment), the WeakMap entry is GC'd, which makes the
      // blob URL unreachable and the browser reclaims it.
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
