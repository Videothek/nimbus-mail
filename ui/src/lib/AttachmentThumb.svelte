<script lang="ts">
  // Square inline thumbnail for an attachment — renders a real
  // image preview when the attachment is an image (bytes already
  // in memory, so we can build a blob URL synchronously) and
  // falls through to <FileTypeIcon> for everything else.
  //
  // Used by Compose's attachment chip strip and MailView's
  // attachment chip strip (the latter passes lazily-fetched
  // bytes via `bytesProvider`).

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
  }
  let {
    bytes = null,
    bytesProvider,
    contentType = '',
    filename,
    class: cls = 'w-9 h-9',
  }: Props = $props()

  function isImage(): boolean {
    const ct = (contentType ?? '').toLowerCase()
    if (ct.startsWith('image/')) return true
    const dot = filename.lastIndexOf('.')
    const ext = dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
    return ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'tif', 'tiff', 'heic', 'heif', 'svg'].includes(ext)
  }

  let blobUrl = $state<string | null>(null)

  function makeBlobUrl(b: Uint8Array | number[]): string {
    const ct = contentType && contentType.startsWith('image/') ? contentType : 'image/png'
    const u8 = b instanceof Uint8Array ? b : new Uint8Array(b)
    return URL.createObjectURL(new Blob([u8], { type: ct }))
  }

  $effect(() => {
    if (!isImage()) {
      blobUrl = null
      return
    }
    let cancelled = false
    let createdUrl: string | null = null
    if (bytes && bytes.length > 0) {
      createdUrl = makeBlobUrl(bytes)
      blobUrl = createdUrl
    } else if (bytesProvider) {
      void (async () => {
        try {
          const b = await bytesProvider()
          if (cancelled) return
          if (!b || b.length === 0) {
            blobUrl = null
            return
          }
          createdUrl = makeBlobUrl(b)
          blobUrl = createdUrl
        } catch (e) {
          console.warn('attachment thumb load failed', e)
        }
      })()
    } else {
      blobUrl = null
    }
    return () => {
      cancelled = true
      if (createdUrl) URL.revokeObjectURL(createdUrl)
    }
  })
</script>

{#if blobUrl}
  <img
    src={blobUrl}
    alt=""
    loading="lazy"
    class="{cls} object-cover rounded shrink-0 bg-surface-200 dark:bg-surface-800"
  />
{:else}
  <FileTypeIcon contentType={contentType} filename={filename} class={cls} />
{/if}
