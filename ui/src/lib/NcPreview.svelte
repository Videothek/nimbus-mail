<script lang="ts" module>
  // Session-scoped preview cache shared across every NcPreview
  // instance.  Keyed by `${accountId}::${path}` and stores either
  // a blob URL (success) or null (no preview available — server
  // 404 or unsupported type), so subsequent mounts of the same
  // file skip the IPC entirely.  Blob URLs are intentionally
  // not revoked: the picker is opened for a few seconds at a
  // time and the URLs become stale on app restart.
  const PREVIEW_CACHE = new Map<string, string | null>()
</script>

<script lang="ts">
  // Inline thumbnail for a file in the Nextcloud picker.  For
  // image and video rows it lazy-fetches the server-rendered
  // preview from `/index.php/core/preview.png?...` via the
  // `nextcloud_file_preview` Tauri command and renders an
  // `<img>` once the bytes land; everything else falls through
  // to <FileTypeIcon>.
  import { invoke } from '@tauri-apps/api/core'
  import FileTypeIcon from './FileTypeIcon.svelte'
  import AttachmentThumb from './AttachmentThumb.svelte'

  interface Props {
    accountId: string
    path: string
    contentType?: string | null
    filename: string
    /** Square thumbnail size — Tailwind utility (e.g. `w-9 h-9`). */
    class?: string
    /** File size in bytes, when known.  Used to skip the
     *  client-side video first-frame fallback for files that
     *  are too large to be worth downloading just for a
     *  thumbnail. */
    size?: number | null
  }
  let {
    accountId,
    path,
    contentType = '',
    filename,
    class: cls = 'w-9 h-9',
    size = null,
  }: Props = $props()

  /** Cap for the client-side video poster fallback when NC
   *  doesn't return a server-rendered preview.  Anything above
   *  this stays on the typed icon — downloading a 200 MiB clip
   *  for a 36px chip is never worth it. */
  const VIDEO_FALLBACK_MAX_BYTES = 16 * 1024 * 1024

  function ext(): string {
    const dot = filename.lastIndexOf('.')
    return dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
  }
  function isVideo(): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('video/')) return true
    return ['mp4', 'mkv', 'mov', 'avi', 'webm', 'm4v', 'mpeg', 'mpg', '3gp', 'wmv', 'flv'].includes(
      ext(),
    )
  }
  function isPreviewable(): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('image/') || ct.startsWith('video/')) return true
    return [
      'jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'tif', 'tiff', 'heic', 'heif',
      'mp4', 'mkv', 'mov', 'avi', 'webm', 'm4v', 'mpeg', 'mpg', '3gp', 'wmv', 'flv',
    ].includes(ext())
  }
  /** True when we should fall back to client-side first-frame
   *  extraction (download bytes + canvas).  Reserved for video
   *  files NC didn't return a server-rendered poster for, and
   *  only when the file isn't huge. */
  function shouldFallbackToBytes(): boolean {
    if (!isVideo()) return false
    if (size != null && size > VIDEO_FALLBACK_MAX_BYTES) return false
    return true
  }

  let previewUrl = $state<string | null>(null)
  /** Set when NC didn't return a server-rendered poster for a
   *  video file and we want to hand the rendering off to
   *  AttachmentThumb's client-side first-frame extractor. */
  let useBytesFallback = $state(false)
  $effect(() => {
    if (!isPreviewable()) {
      previewUrl = null
      useBytesFallback = false
      return
    }
    const key = `${accountId}::${path}`
    if (PREVIEW_CACHE.has(key)) {
      previewUrl = PREVIEW_CACHE.get(key) ?? null
      useBytesFallback = previewUrl === null && shouldFallbackToBytes()
      return
    }
    let cancelled = false
    void (async () => {
      try {
        const bytes = await invoke<number[] | null>('nextcloud_file_preview', {
          ncId: accountId,
          path,
          size: 128,
        })
        if (cancelled) return
        if (!bytes || bytes.length === 0) {
          PREVIEW_CACHE.set(key, null)
          useBytesFallback = shouldFallbackToBytes()
          return
        }
        // Nextcloud's preview endpoint always re-encodes to PNG
        // regardless of source format, so the MIME we hand to
        // the Blob is fine to hardcode.
        const blob = new Blob([new Uint8Array(bytes)], { type: 'image/png' })
        const url = URL.createObjectURL(blob)
        PREVIEW_CACHE.set(key, url)
        previewUrl = url
      } catch (e) {
        if (!cancelled) {
          console.warn('nextcloud_file_preview failed', e)
          PREVIEW_CACHE.set(key, null)
          useBytesFallback = shouldFallbackToBytes()
        }
      }
    })()
    return () => {
      cancelled = true
    }
  })
</script>

{#if previewUrl}
  <img
    src={previewUrl}
    alt=""
    loading="lazy"
    class="{cls} object-cover rounded shrink-0 bg-surface-200 dark:bg-surface-800"
  />
{:else if useBytesFallback}
  <!-- NC didn't return a server-side poster (no previewgenerator
       installed, or simply not generated yet for this file).
       Pull the bytes down once and let AttachmentThumb pull a
       first frame off the moov atom client-side; cached, so
       only the first reach actually downloads. -->
  <AttachmentThumb
    contentType={contentType}
    filename={filename}
    cacheKey={`nc::${accountId}::${path}`}
    bytesProvider={() =>
      invoke<number[]>('download_nextcloud_file', { ncId: accountId, path })}
    class={cls}
  />
{:else}
  <FileTypeIcon contentType={contentType} filename={filename} class={cls} />
{/if}
