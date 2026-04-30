<script lang="ts">
  // Reusable iOS-style switch.  Replaces native checkboxes
  // across Settings (#164 follow-up) for a lighter, more modern
  // look.  Smaller than the original Security toggle (h-5 / w-9
  // vs h-6 / w-11) so it sits comfortably on dense settings
  // rows without dominating the line.
  //
  // Use:
  //   <Toggle bind:checked={value} label="Enable feature" />
  //
  // Accessible: rendered as a real <button role="switch"> with
  // aria-checked, focusable, Space / Enter activates.

  interface Props {
    checked: boolean
    /** Optional aria label.  When omitted callers should
     *  associate the toggle with surrounding text via context. */
    label?: string
    disabled?: boolean
    onchange?: (checked: boolean) => void
    class?: string
  }
  let {
    checked = $bindable(),
    label = '',
    disabled = false,
    onchange,
    class: cls = '',
  }: Props = $props()

  function flip() {
    if (disabled) return
    checked = !checked
    onchange?.(checked)
  }
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label || undefined}
  disabled={disabled || undefined}
  onclick={flip}
  class="relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors duration-150
         focus:outline-none focus:ring-2 focus:ring-primary-500 focus:ring-offset-2
         focus:ring-offset-surface-50 dark:focus:ring-offset-surface-900
         disabled:opacity-50 disabled:cursor-not-allowed
         {checked ? 'bg-primary-500' : 'bg-surface-300 dark:bg-surface-600'}
         {cls}"
>
  <span
    class="inline-block h-4 w-4 transform rounded-full bg-white shadow-sm transition-transform duration-150
           {checked ? 'translate-x-[18px]' : 'translate-x-0.5'}"
  ></span>
</button>
