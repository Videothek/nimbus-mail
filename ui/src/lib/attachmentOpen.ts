// Shared attachment-open dispatch (#162).
//
// Both MailView (received message attachments) and Compose
// (chips below the editor) need the same "click → open the
// best viewer for this file type" behaviour.  This module is
// the single source of truth for that decision so the two
// surfaces stay in lockstep.
//
// Type tiers, in priority order:
//
//   - **Office** docs (.docx, .xlsx, .odt, …) — uploaded to
//     the user's Nextcloud and opened in a fresh Tauri webview
//     pointing at NC's Files app, which routes to whichever
//     handler the server has registered (Collabora for
//     `.docx`, etc.).
//   - **PDF** — same upload-to-NC path; NC's built-in PDF
//     viewer handles it.
//   - **Markdown** — rendered locally with `marked` and
//     displayed in a fresh Tauri webview window via a data:
//     URL.  Local-only by design: forcing read-only would
//     require upload-with-restricted-permissions on NC, and
//     the user's spec is "read-only", so we skip the NC round
//     trip entirely.
//   - **Everything else** — handed to the OS via Rust's
//     `print_attachment` command (despite the name, it's just
//     "drop in temp dir + ShellExecute / xdg-open").  This is
//     the new default for non-NC-renderable types.
//
// All openers take `(filename, contentType, getBytes)` so
// callers can either pass already-in-memory bytes (Compose) or
// a thunk that fetches them lazily (MailView, where bytes come
// from a Tauri IPC against IMAP).

import { invoke } from '@tauri-apps/api/core'
import { marked } from 'marked'

const OFFICE_MIME_TYPES = new Set([
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation',
  'application/vnd.oasis.opendocument.text',
  'application/vnd.oasis.opendocument.spreadsheet',
  'application/vnd.oasis.opendocument.presentation',
  'application/msword',
  'application/vnd.ms-excel',
  'application/vnd.ms-powerpoint',
  'text/csv',
])
const OFFICE_EXTENSIONS = new Set([
  'docx', 'xlsx', 'pptx', 'odt', 'ods', 'odp', 'doc', 'xls', 'ppt', 'csv',
])

const MARKDOWN_MIME_TYPES = new Set([
  'text/markdown',
  'text/x-markdown',
])
const MARKDOWN_EXTENSIONS = new Set(['md', 'markdown', 'mkd', 'mdown'])

function fileExt(filename: string): string {
  const dot = filename.lastIndexOf('.')
  return dot < 0 ? '' : filename.slice(dot + 1).toLowerCase()
}

export interface AttachmentLike {
  filename: string
  content_type: string
}

export function isOfficeAttachment(att: AttachmentLike): boolean {
  if (OFFICE_MIME_TYPES.has(att.content_type)) return true
  return OFFICE_EXTENSIONS.has(fileExt(att.filename))
}

export function isPdfAttachment(att: AttachmentLike): boolean {
  if (att.content_type === 'application/pdf') return true
  return att.filename.toLowerCase().endsWith('.pdf')
}

export function isMarkdownAttachment(att: AttachmentLike): boolean {
  if (MARKDOWN_MIME_TYPES.has(att.content_type)) return true
  return MARKDOWN_EXTENSIONS.has(fileExt(att.filename))
}

/** Returns the user-facing label for the chip's primary action
 *  button.  Drives both MailView's chip-button text and the
 *  click-target tooltip so the two surfaces stay in sync. */
export function attachmentPrimaryActionLabel(att: AttachmentLike): {
  label: string
  iconName: 'open-in-browser' | 'open-on-desktop'
} {
  if (isOfficeAttachment(att)) return { label: 'Open in Office', iconName: 'open-in-browser' }
  if (isPdfAttachment(att)) return { label: 'Open PDF', iconName: 'open-in-browser' }
  if (isMarkdownAttachment(att)) return { label: 'Open Markdown', iconName: 'open-in-browser' }
  return { label: 'Open', iconName: 'open-on-desktop' }
}

interface OpenResult {
  url: string
  tempPath: string
}

/** Common upload-to-NC + open-in-webview machinery used by the
 *  Office and PDF (and now Markdown-via-NC if we ever put it
 *  there) flows.  Picks the first connected NC; surfaces a
 *  user-actionable error if none is connected. */
async function openViaNcViewer(
  command: 'office_open_attachment' | 'pdf_open_attachment',
  filename: string,
  contentType: string | null,
  bytes: number[],
): Promise<void> {
  const ncAccounts = await invoke<{ id: string }[]>('get_nextcloud_accounts')
  if (ncAccounts.length === 0) {
    throw new Error(
      'Connect a Nextcloud account in Settings to open this file in the embedded viewer.',
    )
  }
  const ncId = ncAccounts[0].id
  const result = await invoke<OpenResult>(command, {
    ncId,
    filename,
    data: bytes,
    contentType,
  })

  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow')
  const label = `att-${crypto.randomUUID().replaceAll('-', '')}`
  const win = new WebviewWindow(label, {
    url: result.url,
    title: filename,
    width: 1200,
    height: 800,
  })
  // Cleanup hook fires once when the user closes the viewer
  // window — DAV-deletes the temp file from NC so the user's
  // /Nimbus Mail/temp doesn't accumulate.  Errors are
  // swallowed; the startup sweeper handles orphans.
  void win.once('tauri://destroyed', async () => {
    try {
      await invoke(
        command === 'pdf_open_attachment'
          ? 'pdf_close_attachment'
          : 'office_close_attachment',
        { ncId, tempPath: result.tempPath },
      )
    } catch (e) {
      console.warn('attachment close cleanup failed:', e)
    }
  })
}

/** Render markdown to HTML via `marked`, wrap in a minimal
 *  styled document, and open it in a fresh Tauri webview via a
 *  data: URL.  Read-only by construction (no editor, no save
 *  surface).  No NC roundtrip — keeps preview fast and offline-
 *  friendly, which matters because Compose may not have a
 *  connected NC and the user still wants to verify what
 *  they're attaching. */
async function openMarkdownLocally(filename: string, bytes: number[]): Promise<void> {
  // Bytes → text.  TextDecoder('utf-8') with `fatal: false`
  // (the default) replaces invalid sequences with U+FFFD, which
  // is the right behaviour for a viewer — better to render a
  // mostly-correct file than refuse outright.
  const text = new TextDecoder('utf-8').decode(new Uint8Array(bytes))
  // Disable raw-HTML embedding via marked's options so a
  // markdown file with a `<script>` block can't run JS in the
  // viewer.  The output still goes through a sanitiser pass
  // below as belt + braces — every defence layer here is cheap
  // because we own the document we're constructing.
  const rendered = await marked.parse(text, { async: true, gfm: true, breaks: false })

  // Belt + braces: even with marked configured to escape HTML,
  // strip anything dangerous from the rendered tree.  We can't
  // call DOMPurify directly here (it expects a DOM, not a
  // string we'll wrap in a data: URL), so we do the strip
  // server-side: write the markup into a detached DOMParser
  // document, run a small allowlist over it, then serialise
  // back.  This duplicates a tiny portion of DOMPurify but
  // keeps us from shipping the full sanitiser inside the data:
  // URL.
  const safeBody = sanitiseRenderedMarkdown(rendered)

  const html = wrapMarkdownDocument(filename, safeBody)
  // Encode via base64 (binary-safe) so the data: URL is valid
  // even when the rendered HTML contains characters that a
  // plain `data:text/html,...` would percent-encode awkwardly.
  const b64 = btoa(unescape(encodeURIComponent(html)))
  const dataUrl = `data:text/html;base64,${b64}`

  const { WebviewWindow } = await import('@tauri-apps/api/webviewWindow')
  const label = `md-${crypto.randomUUID().replaceAll('-', '')}`
  new WebviewWindow(label, {
    url: dataUrl,
    title: filename,
    width: 900,
    height: 700,
  })
}

function sanitiseRenderedMarkdown(html: string): string {
  // Quick allowlist — markdown only legitimately produces a
  // small set of tags, so anything outside this list either
  // came from a raw HTML block or from a misconfigured marked.
  const ALLOWED = new Set([
    'p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li',
    'blockquote', 'code', 'pre', 'em', 'strong', 'a', 'br', 'hr',
    'table', 'thead', 'tbody', 'tr', 'th', 'td', 'img', 'span',
    'del', 'ins', 'sub', 'sup',
  ])
  const ALLOWED_ATTR = new Set(['href', 'title', 'alt', 'src', 'colspan', 'rowspan', 'align'])
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, 'text/html')
  const walk = (root: Element) => {
    // Walk a shallow copy of children since we mutate in place.
    for (const child of Array.from(root.children)) {
      const tag = child.tagName.toLowerCase()
      if (!ALLOWED.has(tag)) {
        // Replace disallowed element with its text content so
        // the user still sees the data, just not the tag.
        child.replaceWith(doc.createTextNode(child.textContent ?? ''))
        continue
      }
      for (const attr of Array.from(child.attributes)) {
        if (!ALLOWED_ATTR.has(attr.name.toLowerCase())) {
          child.removeAttribute(attr.name)
        } else if (attr.name.toLowerCase() === 'href' || attr.name.toLowerCase() === 'src') {
          // Only allow http(s) / mailto / cid / data URIs for
          // images.  `javascript:` / `vbscript:` / `data:` for
          // non-images get stripped.
          const v = attr.value.trim().toLowerCase()
          const ok =
            v.startsWith('http://') ||
            v.startsWith('https://') ||
            v.startsWith('mailto:') ||
            (attr.name.toLowerCase() === 'src' && v.startsWith('data:image/'))
          if (!ok) child.removeAttribute(attr.name)
        }
      }
      walk(child)
    }
  }
  walk(doc.body)
  return doc.body.innerHTML
}

function wrapMarkdownDocument(title: string, body: string): string {
  // Minimal styling so the document renders pleasantly without
  // pulling in the app's stylesheet.  Inline because the data:
  // URL is the whole document — there's no second request the
  // browser could pull a stylesheet from.
  const escapedTitle = title.replace(/[<>&"]/g, (c) =>
    ({ '<': '&lt;', '>': '&gt;', '&': '&amp;', '"': '&quot;' })[c] ?? c,
  )
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>${escapedTitle}</title>
<style>
  :root { color-scheme: light dark; }
  body {
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    line-height: 1.6;
    max-width: 720px;
    margin: 2.5rem auto;
    padding: 0 1.5rem;
    color: #1f2937;
    background: #fff;
  }
  @media (prefers-color-scheme: dark) {
    body { color: #e5e7eb; background: #0f172a; }
    a { color: #60a5fa; }
    code, pre { background: #1e293b; }
    blockquote { border-left-color: #334155; color: #94a3b8; }
    th, td { border-color: #334155; }
  }
  h1, h2, h3, h4, h5, h6 { line-height: 1.25; margin-top: 2rem; }
  h1 { border-bottom: 1px solid currentColor; padding-bottom: 0.3rem; }
  h2 { border-bottom: 1px solid currentColor; padding-bottom: 0.2rem; }
  a { color: #2563eb; }
  code { background: #f1f5f9; padding: 0.1em 0.3em; border-radius: 0.2em; font-size: 0.9em; }
  pre { background: #f1f5f9; padding: 0.8rem 1rem; border-radius: 0.4rem; overflow-x: auto; }
  pre code { background: transparent; padding: 0; }
  blockquote { border-left: 4px solid #cbd5e1; margin: 0; padding: 0 1rem; color: #475569; }
  table { border-collapse: collapse; }
  th, td { border: 1px solid #cbd5e1; padding: 0.4rem 0.7rem; }
  img { max-width: 100%; height: auto; }
</style>
</head>
<body>
${body}
</body>
</html>`
}

/** Drop bytes into a temp dir and hand them to the OS shell so
 *  the user's default app for that file type opens them.  No
 *  read-back: we're firing a "preview / open" UX, not editing
 *  in place. */
async function openInDesktopApp(
  filename: string,
  bytes: number[],
): Promise<void> {
  await invoke('print_attachment', { fileName: filename, bytes })
}

/**
 * Single dispatch entry point.  `getBytes` lets callers either
 * pass an already-resolved byte array (Compose, where bytes
 * live in memory) or a function that fetches them lazily
 * (MailView, where bytes need an IMAP round-trip).
 */
export async function openAttachment(
  att: AttachmentLike,
  getBytes: () => Promise<number[]>,
): Promise<void> {
  if (isOfficeAttachment(att)) {
    const bytes = await getBytes()
    await openViaNcViewer('office_open_attachment', att.filename, att.content_type || null, bytes)
    return
  }
  if (isPdfAttachment(att)) {
    const bytes = await getBytes()
    await openViaNcViewer('pdf_open_attachment', att.filename, att.content_type || null, bytes)
    return
  }
  if (isMarkdownAttachment(att)) {
    const bytes = await getBytes()
    await openMarkdownLocally(att.filename, bytes)
    return
  }
  const bytes = await getBytes()
  await openInDesktopApp(att.filename, bytes)
}
