<script lang="ts">
  // Small coloured chip for vCard email-kind labels (HOME / WORK
  // / CELL / OTHER / …) shown next to an address in autocomplete
  // dropdowns.  Colours are Skeleton theme tokens, so the chip
  // re-tints automatically when the user switches themes.
  //
  // Visual: rounded-full pill, 10px uppercase semibold text,
  // subtle alpha background with matching foreground colour.
  // Same shape we use for source pills on the Mailing Lists tab
  // so the chip language reads consistently across the app.

  interface Props {
    /** vCard kind string — `HOME`, `WORK`, `CELL`, `OTHER`,
     *  `INTERNET`, etc.  Case-insensitive. */
    kind?: string | null
    /** Optional extra classes (margins / inline-block flags). */
    class?: string
  }
  let { kind = '', class: cls = '' }: Props = $props()

  const meta = $derived.by(() => {
    const k = (kind ?? '').toLowerCase()
    if (!k) return null
    if (k.includes('work'))
      return {
        label: 'Work',
        classes: 'bg-primary-500/15 text-primary-700 dark:text-primary-300',
      }
    if (k.includes('home'))
      return {
        label: 'Home',
        classes: 'bg-success-500/15 text-success-700 dark:text-success-300',
      }
    if (k.includes('cell') || k.includes('mobile'))
      return {
        label: 'Mobile',
        classes: 'bg-secondary-500/15 text-secondary-700 dark:text-secondary-300',
      }
    if (k.includes('fax'))
      return {
        label: 'Fax',
        classes: 'bg-warning-500/15 text-warning-700 dark:text-warning-300',
      }
    if (k.includes('internet'))
      return {
        label: 'Internet',
        classes: 'bg-tertiary-500/15 text-tertiary-700 dark:text-tertiary-300',
      }
    return {
      label: kind ?? 'Other',
      classes:
        'bg-surface-300/40 dark:bg-surface-700/40 text-surface-700 dark:text-surface-300',
    }
  })
</script>

{#if meta}
  <span
    class="inline-block px-2 py-[1px] rounded-full text-[10px] font-semibold uppercase tracking-wide leading-tight align-middle {meta.classes} {cls}"
  >{meta.label}</span>
{/if}
