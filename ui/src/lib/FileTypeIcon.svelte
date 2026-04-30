<script lang="ts">
  // Inline-SVG file-type icons (#93 follow-up).  Replaces the
  // generic emoji `attachmentIcon()` for document/office/archive
  // types with a recognisable per-format glyph: a Tabler-style
  // file outline (corner fold) with a small text label inside —
  // PDF, DOC, XLS, PPT, CSV, ZIP — tinted in a colour the user
  // already associates with that format (red for PDF, blue for
  // Word, green for Excel, orange for PowerPoint, violet for ZIP).
  //
  // Why inline SVG instead of a Tabler/Phosphor npm package:
  // - Zero dep churn, no build-tool surprises.
  // - Tree-shaking is automatic — only the icons we actually
  //   render ship.
  // - Matches the existing pattern in CalendarView's pin SVG.
  //
  // Media types (image / video / audio) keep their emoji glyphs;
  // they're visually distinct enough that a colour-coded label
  // doesn't add anything.

  interface Props {
    contentType?: string | null
    filename?: string
    /** Tailwind sizing classes; defaults to `w-4 h-4` for the
     *  16px attachment chip but the Files browser uses `w-5 h-5`. */
    class?: string
  }
  let { contentType = '', filename = '', class: cls = 'w-4 h-4' }: Props = $props()

  type Kind = {
    /** Three-letter label rendered inside the file outline. */
    label: string
    /** Tailwind colour class — applied to stroke + fill via
     *  `currentColor`, so the SVG inherits it. */
    colorClass: string
  }

  function detectKind(): Kind | null {
    const ct = (contentType ?? '').toLowerCase()
    const dot = filename.lastIndexOf('.')
    const ext = dot >= 0 ? filename.slice(dot + 1).toLowerCase() : ''
    if (ct.includes('pdf') || ext === 'pdf')
      return { label: 'PDF', colorClass: 'text-rose-500' }
    if (
      ct.includes('msword') ||
      ct.includes('officedocument.wordprocessing') ||
      ct.includes('opendocument.text') ||
      ['doc', 'docx', 'docm', 'dot', 'dotx', 'dotm', 'odt', 'ott', 'rtf'].includes(ext)
    )
      return { label: 'DOC', colorClass: 'text-blue-500' }
    if (ct.includes('csv') || ['csv', 'tsv'].includes(ext))
      return { label: 'CSV', colorClass: 'text-emerald-500' }
    if (
      ct.includes('ms-excel') ||
      ct.includes('officedocument.spreadsheet') ||
      ct.includes('opendocument.spreadsheet') ||
      ['xls', 'xlsx', 'xlsm', 'xlt', 'xltx', 'xltm', 'ods', 'ots'].includes(ext)
    )
      return { label: 'XLS', colorClass: 'text-emerald-600' }
    if (
      ct.includes('ms-powerpoint') ||
      ct.includes('officedocument.presentation') ||
      ct.includes('opendocument.presentation') ||
      ['ppt', 'pptx', 'pptm', 'pot', 'potx', 'potm', 'odp', 'otp'].includes(ext)
    )
      return { label: 'PPT', colorClass: 'text-amber-500' }
    if (
      ct.includes('zip') ||
      ct.includes('compressed') ||
      ['zip', '7z', 'rar', 'tar', 'gz', 'xz', 'bz2', 'tgz'].includes(ext)
    )
      return { label: 'ZIP', colorClass: 'text-violet-500' }
    // Markdown — render as `MD` in a distinct sky tone so a
    // README in an attachment list jumps out from the surrounding
    // plain-text files.
    if (ct.includes('markdown') || ['md', 'markdown', 'mdx', 'mkd'].includes(ext))
      return { label: 'MD', colorClass: 'text-sky-500' }
    // Images — show the format code (PNG / JPG / GIF / SVG / etc.)
    // rather than a generic photo glyph, so a thumbnail strip
    // tells you at a glance which format each row is.
    if (ct.startsWith('image/')) {
      const sub = ct.slice('image/'.length).split(';')[0].trim()
      const fromCt = sub === 'jpeg' ? 'JPG' : sub === 'svg+xml' ? 'SVG' : sub.toUpperCase()
      const label = (fromCt || 'IMG').slice(0, 4)
      return { label, colorClass: 'text-cyan-500' }
    }
    if (
      ['jpg', 'jpeg', 'png', 'gif', 'webp', 'avif', 'bmp', 'tif', 'tiff', 'svg', 'heic', 'heif', 'ico'].includes(ext)
    ) {
      const label = ext === 'jpeg' ? 'JPG' : ext.toUpperCase()
      return { label: label.slice(0, 4), colorClass: 'text-cyan-500' }
    }
    // Video — pink to stand apart from the doc/archive palette.
    if (ct.startsWith('video/')) {
      const sub = ct.slice('video/'.length).split(';')[0].trim()
      const label = (sub || 'VID').toUpperCase().slice(0, 4)
      return { label, colorClass: 'text-pink-500' }
    }
    if (['mp4', 'mkv', 'mov', 'avi', 'wmv', 'flv', 'webm', 'm4v', 'mpg', 'mpeg', '3gp'].includes(ext)) {
      return { label: ext.toUpperCase().slice(0, 4), colorClass: 'text-pink-500' }
    }
    // Audio — purple, distinct from the cyan/sky/pink trio.
    if (ct.startsWith('audio/')) {
      const sub = ct.slice('audio/'.length).split(';')[0].trim()
      const label = (sub || 'AUD').toUpperCase().slice(0, 4)
      return { label, colorClass: 'text-purple-500' }
    }
    if (['mp3', 'flac', 'wav', 'ogg', 'm4a', 'aac', 'opus', 'wma', 'aiff', 'alac'].includes(ext)) {
      return { label: ext.toUpperCase().slice(0, 4), colorClass: 'text-purple-500' }
    }
    return null
  }

  const kind = $derived(detectKind())
</script>

{#if kind}
  <!-- Minimalist file mark (#158 v2).  Apple Files / Notion-
       style layout: a clean light document body with a subtle
       outline + corner fold up top, and the format code in
       white inside a coloured band at the bottom.  Keeping the
       text in its own band means the corner fold never bumps
       into the letters — readable at every size. -->
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 24 24"
    class="{cls} {kind.colorClass} shrink-0"
    aria-label={kind.label}
  >
    <!-- Document body — neutral white-ish fill, subtle border
         in the format colour.  We use `currentColor` for the
         border + bottom band so the icon re-tints with the
         text-* utility class on the parent. -->
    <path
      d="M6.5 2.5h7L19 8v12.5a1.5 1.5 0 0 1 -1.5 1.5h-11A1.5 1.5 0 0 1 5 20.5v-16.5A1.5 1.5 0 0 1 6.5 2.5z"
      fill="white"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linejoin="round"
    />
    <!-- Corner fold — small triangle, just the document
         affordance, never crowds the bottom band. -->
    <path
      d="M13.5 2.5v4.5a1 1 0 0 0 1 1H19"
      fill="none"
      stroke="currentColor"
      stroke-width="1.2"
      stroke-linejoin="round"
    />
    <!-- Format band along the bottom — solid colour with
         enough rounding to feel like part of the document. -->
    <path
      d="M5 14.5h14v6a1.5 1.5 0 0 1 -1.5 1.5h-11A1.5 1.5 0 0 1 5 20.5z"
      fill="currentColor"
    />
    <!-- Format label, white on the band -->
    <text
      x="12"
      y="19.4"
      text-anchor="middle"
      font-size="4.8"
      font-weight="800"
      letter-spacing="0.4"
      fill="white"
      stroke="none"
      font-family="ui-sans-serif, system-ui, -apple-system, sans-serif"
    >{kind.label}</text>
  </svg>
{:else}
  <!-- Untyped fallback — same silhouette in neutral surface
       tone, no bottom band (no format to advertise). -->
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 24 24"
    class="{cls} text-surface-400 dark:text-surface-500 shrink-0"
    aria-hidden="true"
  >
    <path
      d="M6.5 2.5h7L19 8v12.5a1.5 1.5 0 0 1 -1.5 1.5h-11A1.5 1.5 0 0 1 5 20.5v-16.5A1.5 1.5 0 0 1 6.5 2.5z"
      fill="white"
      stroke="currentColor"
      stroke-width="1.4"
      stroke-linejoin="round"
    />
    <path
      d="M13.5 2.5v4.5a1 1 0 0 0 1 1H19"
      fill="none"
      stroke="currentColor"
      stroke-width="1.2"
      stroke-linejoin="round"
    />
  </svg>
{/if}
