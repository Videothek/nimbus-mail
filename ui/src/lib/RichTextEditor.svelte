<script lang="ts">
  /**
   * RichTextEditor — Tiptap-based WYSIWYG editor for composing emails.
   *
   * Provides an Outlook-style toolbar with formatting controls: text
   * styles (bold, italic, underline, strikethrough), headings, lists,
   * alignment, links, tables, colors, horizontal rules, and images.
   *
   * The editor exposes its HTML content reactively via `onchange` so the
   * parent (Compose) can read it at send time.
   */

  import { onDestroy } from 'svelte'
  import { createEditor, EditorContent } from 'svelte-tiptap'
  import StarterKit from '@tiptap/starter-kit'
  import Underline from '@tiptap/extension-underline'
  import Link from '@tiptap/extension-link'
  import Image from '@tiptap/extension-image'
  import TextAlign from '@tiptap/extension-text-align'
  import Placeholder from '@tiptap/extension-placeholder'
  import { TextStyle } from '@tiptap/extension-text-style'
  import Color from '@tiptap/extension-color'
  import Highlight from '@tiptap/extension-highlight'
  import { Table } from '@tiptap/extension-table'
  import TableRow from '@tiptap/extension-table-row'
  import TableCell from '@tiptap/extension-table-cell'
  import TableHeader from '@tiptap/extension-table-header'

  interface Props {
    /** Initial HTML content (e.g. quoted reply body). */
    content?: string
    /** Placeholder text shown when the editor is empty. */
    placeholder?: string
    /** Fires on every content change with the current HTML. */
    onchange?: (html: string) => void
  }
  let {
    content = '',
    placeholder = 'Write your message\u2026',
    onchange,
  }: Props = $props()

  // svelte-ignore state_referenced_locally
  const editor = createEditor({
    extensions: [
      StarterKit.configure({
        heading: { levels: [1, 2, 3] },
      }),
      Underline,
      Link.configure({
        openOnClick: false,
        HTMLAttributes: { target: '_blank', rel: 'noopener noreferrer' },
      }),
      Image.configure({ inline: true }),
      TextAlign.configure({ types: ['heading', 'paragraph'] }),
      // svelte-ignore state_referenced_locally
      Placeholder.configure({ placeholder }),
      TextStyle,
      Color,
      Highlight.configure({ multicolor: true }),
      Table.configure({ resizable: true }),
      TableRow,
      TableCell,
      TableHeader,
    ],
    // svelte-ignore state_referenced_locally
    content,
    onUpdate: ({ editor: e }) => {
      onchange?.(e.getHTML())
    },
  })

  onDestroy(() => {
    $editor?.destroy()
  })

  // ── Toolbar helpers ─────────────────────────────────────────

  // ── Toolbar helpers ─────────────────────────────────────────
  // Each helper grabs the editor from the store at call time (not
  // capture time) so it's always the live instance.

  function cmd() {
    return $editor!.chain().focus()
  }

  function toggleHeading(level: 1 | 2 | 3) {
    cmd().toggleHeading({ level }).run()
  }

  function doUndo() { cmd().undo().run() }
  function doRedo() { cmd().redo().run() }

  function setLink() {
    const prev = $editor?.getAttributes('link')?.href ?? ''
    const url = window.prompt('URL', prev)
    if (url === null) return
    if (url === '') {
      cmd().extendMarkRange('link').unsetLink().run()
    } else {
      cmd().extendMarkRange('link').setLink({ href: url }).run()
    }
  }

  /** Insert an image from a local file (embedded as data URL). */
  function addImageFromFile() {
    const input = document.createElement('input')
    input.type = 'file'
    input.accept = 'image/*'
    input.onchange = () => {
      const file = input.files?.[0]
      if (!file) return
      const reader = new FileReader()
      reader.onload = () => {
        const src = reader.result as string
        cmd().setImage({ src }).run()
      }
      reader.readAsDataURL(file)
    }
    input.click()
  }

  /** Insert an image from a URL. */
  function addImageFromUrl() {
    const url = window.prompt('Image URL')
    if (url) {
      cmd().setImage({ src: url }).run()
    }
  }

  // ── Table grid picker state ────────────────────────────────
  let showTablePicker = $state(false)
  let tableHoverRows = $state(0)
  let tableHoverCols = $state(0)
  const TABLE_GRID = 8  // 8x8 picker like Outlook

  function insertTable(rows: number, cols: number) {
    cmd().insertTable({ rows, cols, withHeaderRow: true }).run()
    showTablePicker = false
  }

  function setColor(e: Event) {
    const color = (e.target as HTMLInputElement).value
    cmd().setColor(color).run()
  }

  function setHighlight(e: Event) {
    const color = (e.target as HTMLInputElement).value
    cmd().toggleHighlight({ color }).run()
  }

  // Reactive "is active" helpers for styling toolbar buttons.
  const ACTIVE_CLS = 'bg-surface-300 dark:bg-surface-600'

  function active(name: string, attrs?: Record<string, unknown>): string {
    return $editor?.isActive(name, attrs) ? ACTIVE_CLS : ''
  }

  function activeAttrs(attrs: Record<string, unknown>): string {
    return $editor?.isActive(attrs) ? ACTIVE_CLS : ''
  }
</script>

<style>
  /* Toolbar buttons — small, consistent touch targets. */
  .tb {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0.25rem 0.5rem;
    border-radius: 0.25rem;
    font-size: 0.75rem;
    line-height: 1;
    cursor: pointer;
    transition: background 0.1s;
    position: relative;
    border: none;
    background: transparent;
    color: inherit;
  }
  .tb:hover {
    background: var(--color-surface-200);
  }
  :global(.dark) .tb:hover {
    background: var(--color-surface-700);
  }
  /* Tiptap editor chrome — keep the editing area clean and consistent
     with the rest of the app. */
  :global(.tiptap) {
    outline: none;
    min-height: 200px;
    padding: 0.75rem;
    font-size: 0.875rem;
    line-height: 1.625;
  }
  :global(.tiptap p.is-editor-empty:first-child::before) {
    content: attr(data-placeholder);
    float: left;
    pointer-events: none;
    height: 0;
    color: #9ca3af;
  }
  /* Basic table styling so it's visible in the editor. */
  :global(.tiptap table) {
    border-collapse: collapse;
    width: 100%;
    margin: 0.5rem 0;
  }
  :global(.tiptap th),
  :global(.tiptap td) {
    border: 1px solid #d1d5db;
    padding: 0.375rem 0.625rem;
    text-align: left;
    min-width: 80px;
  }
  :global(.tiptap th) {
    background: #f3f4f6;
    font-weight: 600;
  }
  :global(.tiptap img) {
    max-width: 100%;
    height: auto;
  }
  :global(.tiptap blockquote) {
    border-left: 3px solid #d1d5db;
    padding-left: 0.75rem;
    margin: 0.5rem 0;
    color: #6b7280;
  }
  :global(.tiptap hr) {
    border: none;
    border-top: 1px solid #d1d5db;
    margin: 1rem 0;
  }
  :global(.tiptap ul),
  :global(.tiptap ol) {
    padding-left: 1.5rem;
    margin: 0.25rem 0;
  }
  :global(.tiptap ul) { list-style-type: disc; }
  :global(.tiptap ol) { list-style-type: decimal; }
  :global(.tiptap a) { color: #3b82f6; text-decoration: underline; }
</style>

{#if $editor}
  <!-- Toolbar -->
  <div class="flex flex-wrap items-center gap-0.5 px-2 py-1.5 border-b border-surface-200 dark:border-surface-700 bg-surface-100 dark:bg-surface-800 text-sm">
    <!-- Text style group -->
    <button class="tb {active('bold')}" title="Bold" onclick={() => $editor?.chain().focus().toggleBold().run()}>
      <strong>B</strong>
    </button>
    <button class="tb {active('italic')}" title="Italic" onclick={() => $editor?.chain().focus().toggleItalic().run()}>
      <em>I</em>
    </button>
    <button class="tb {active('underline')}" title="Underline" onclick={() => $editor?.chain().focus().toggleUnderline().run()}>
      <u>U</u>
    </button>
    <button class="tb {active('strike')}" title="Strikethrough" onclick={() => $editor?.chain().focus().toggleStrike().run()}>
      <s>S</s>
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Headings -->
    <button class="tb {active('heading', { level: 1 })}" title="Heading 1" onclick={() => toggleHeading(1)}>
      H1
    </button>
    <button class="tb {active('heading', { level: 2 })}" title="Heading 2" onclick={() => toggleHeading(2)}>
      H2
    </button>
    <button class="tb {active('heading', { level: 3 })}" title="Heading 3" onclick={() => toggleHeading(3)}>
      H3
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Lists -->
    <button class="tb {active('bulletList')}" title="Bullet list" onclick={() => $editor?.chain().focus().toggleBulletList().run()}>
      &#8226; List
    </button>
    <button class="tb {active('orderedList')}" title="Numbered list" onclick={() => $editor?.chain().focus().toggleOrderedList().run()}>
      1. List
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Alignment -->
    <button class="tb {activeAttrs({ textAlign: 'left' })}" title="Align left" onclick={() => $editor?.chain().focus().setTextAlign('left').run()}>
      &#x2261;L
    </button>
    <button class="tb {activeAttrs({ textAlign: 'center' })}" title="Align center" onclick={() => $editor?.chain().focus().setTextAlign('center').run()}>
      &#x2261;C
    </button>
    <button class="tb {activeAttrs({ textAlign: 'right' })}" title="Align right" onclick={() => $editor?.chain().focus().setTextAlign('right').run()}>
      &#x2261;R
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Colors -->
    <label class="tb cursor-pointer" title="Text color">
      A
      <input type="color" class="w-0 h-0 opacity-0 absolute" onchange={setColor} />
    </label>
    <label class="tb cursor-pointer" title="Highlight color">
      <span class="bg-yellow-200 px-0.5 rounded-sm">H</span>
      <input type="color" value="#fde68a" class="w-0 h-0 opacity-0 absolute" onchange={setHighlight} />
    </label>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Insert group -->
    <button class="tb {active('link')}" title="Insert link" onclick={setLink}>
      Link
    </button>

    <!-- Image: dropdown with File / URL options -->
    <div class="relative inline-block">
      <button class="tb" title="Insert image" onclick={() => addImageFromFile()}>
        Image
      </button>
      <button class="tb text-[10px]" title="Insert image from URL" onclick={() => addImageFromUrl()}>
        URL
      </button>
    </div>

    <!-- Table: Outlook-style grid picker -->
    <div class="relative inline-block">
      <button class="tb" title="Insert table" onclick={() => (showTablePicker = !showTablePicker)}>
        Table
      </button>
      {#if showTablePicker}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="absolute left-0 top-full mt-1 z-50 p-2 bg-white dark:bg-surface-800 border border-surface-300 dark:border-surface-600 rounded-md shadow-lg"
          onmouseleave={() => { tableHoverRows = 0; tableHoverCols = 0 }}
        >
          <div class="text-xs text-surface-500 mb-1 text-center">
            {tableHoverRows > 0 ? `${tableHoverRows} × ${tableHoverCols}` : 'Pick size'}
          </div>
          <div class="grid gap-0.5" style="grid-template-columns: repeat({TABLE_GRID}, 1fr)">
            {#each { length: TABLE_GRID } as _, r}
              {#each { length: TABLE_GRID } as _, c}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div
                  class="w-4 h-4 border rounded-sm cursor-pointer transition-colors
                    {r < tableHoverRows && c < tableHoverCols
                      ? 'bg-primary-500/40 border-primary-500'
                      : 'bg-surface-100 dark:bg-surface-700 border-surface-300 dark:border-surface-600'}"
                  onmouseenter={() => { tableHoverRows = r + 1; tableHoverCols = c + 1 }}
                  onclick={() => insertTable(r + 1, c + 1)}
                  role="button"
                  tabindex="-1"
                ></div>
              {/each}
            {/each}
          </div>
        </div>
      {/if}
    </div>

    <button class="tb" title="Horizontal rule" onclick={() => cmd().setHorizontalRule().run()}>
      &#x2015;
    </button>
    <button class="tb {active('blockquote')}" title="Blockquote" onclick={() => cmd().toggleBlockquote().run()}>
      &#x201C;
    </button>

    <span class="w-px h-5 bg-surface-300 dark:bg-surface-600 mx-1"></span>

    <!-- Undo / Redo -->
    <button class="tb" title="Undo (Ctrl+Z)" onclick={() => doUndo()}>
      &#x21A9;
    </button>
    <button class="tb" title="Redo (Ctrl+Y)" onclick={() => doRedo()}>
      &#x21AA;
    </button>
  </div>

  <!-- Editor area -->
  <div class="border border-surface-200 dark:border-surface-700 rounded-b-md bg-white dark:bg-surface-950 overflow-y-auto" style="max-height: 360px;">
    <EditorContent editor={$editor} />
  </div>
{/if}
