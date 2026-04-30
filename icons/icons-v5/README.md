# Mail Client Icons — Set v5

5 additional icons. Combined total: **104 icons**.

## What's new

- **`group`** — informal cluster of people (reuses the `meetings` base — two people, asymmetric)
- **`address-book`** — bound book with a person silhouette inside (the classic phonebook metaphor)
- **`team`** — three people in a row, symmetric and structured (intentionally distinct from `group`)
- **`location`** — standard map pin
- **`time`** — circle + clock hands

## Note on `time` and `away`

These two icons are **visually identical by design** (both are circle + clock hands at 7-12-2 o'clock). The clock-with-hands is the universal time metaphor, and there's no way to draw "time generally" that's distinct from "this person is away" without inventing a non-standard symbol.

In practice this is fine — the surrounding context (a presence indicator next to a name vs. a timestamp label in an event row) tells the user what each one means. If you ever want them to look different, swap one of them for an analog-clock-style outline (no outer circle) or an hourglass.

## Note on `group` vs `team` vs `meetings` vs `contacts`

These four people-icons are intentionally arranged on a spectrum:

| Icon | Composition | Use for |
|---|---|---|
| `contacts` | One person with full body | Individual contact, profile, "my account" |
| `meetings` | Two people, one larger | Meetings, 1:1 conversations |
| `group` | Two people similar size | Informal groups, mailing lists, group chats |
| `team` | Three people in a row | Teams, departments, structured org units |

If your app only uses two of these, the others can be removed. They're separate so you have the option.

## Installation

Same as before — drop `svelte/icons/*.svelte` into `src/lib/icons/` and replace `Icon.svelte` with the v5 version that registers all 104 icons.
