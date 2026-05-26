# Form fields

## Purpose

Text inputs, selects, and toggles for configuration UIs (lua-driven settings panes, dialog forms). Mote intentionally has few of these — most configuration happens by editing `init.lua` — but they exist for prompts, plugin settings panes, and dialogs.

## Variants

| Variant | Use |
|---|---|
| input | text input |
| select | dropdown (native `<select>` styled) |
| textarea | multi-line text |
| toggle | boolean on/off (no checkbox) |
| segmented | small set of mutually exclusive options |

## Structure

```html
<div class="field">
  <label for="config-path">config path</label>
  <input id="config-path" value="~/.config/mote/init.lua" />
</div>

<div class="field">
  <label for="theme-select">theme</label>
  <select id="theme-select">
    <option>dusk</option>
    <option>vellum</option>
  </select>
</div>

<div class="toggle">
  <button class="switch on" role="switch" aria-checked="true"></button>
  <span class="label">enable vim mode</span>
  <span class="reflect">vim_mode = true</span>
</div>
```

## Tokens

```css
.field { display: flex; flex-direction: column; gap: 6px; }
.field label {
  font: var(--text-mono-sm);
  color: var(--fg-2);
  text-transform: uppercase;
  letter-spacing: 0.06em;
}
.field input, .field select, .field textarea {
  background: var(--surface-sunk);
  border: 1px solid var(--border);
  color: var(--fg);
  border-radius: var(--radius-1);
  padding: 7px 10px;
  font: var(--text-mono);
}
.field input:focus, .field select:focus {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
  border-color: var(--accent);
}

/* Toggle */
.switch {
  width: 28px; height: 16px;
  background: var(--surface-3);
  border: 1px solid var(--border);
  border-radius: 9999px;   /* exception to the no-pill rule: status indicators */
  position: relative;
}
.switch::after {
  content: "";
  position: absolute; top: 1px; left: 1px;
  width: 12px; height: 12px;
  border-radius: 50%;
  background: var(--fg-2);
  transition: left var(--dur-base) var(--ease-out);
}
.switch.on        { background: var(--accent); border-color: var(--accent-deep); }
.switch.on::after { left: 13px; background: var(--accent-on); }
```

## States

| State | Style |
|---|---|
| default | surface-sunk bg, border |
| focus | amber outline, amber border |
| filled (input) | fg color |
| disabled | `opacity: 0.4`, `pointer-events: none` |
| error | border becomes `var(--danger)`, error message below in `var(--danger)` mono-sm |

## The "reflect" affordance

Many Mote settings show the **lua equivalent** of the user's change next to the field. This isn't decorative — it teaches users that the form is a thin GUI over their `init.lua`.

```html
<span class="reflect">vim_mode = true</span>
```

```css
.reflect {
  margin-left: auto;
  font: var(--text-mono-sm);
  color: var(--fg-2);
}
```

When the user changes the field, the reflect updates live, and a button (`apply` or `copy`) lets the user write the change to disk or copy the lua to clipboard.

## Behavior

- **Inputs:** `Enter` commits, `Esc` cancels and restores.
- **Selects:** native dropdown.
- **Toggles:** `Space` or `Enter` toggles when focused. `←` / `→` also toggle (`role="switch"` default).

## Accessibility

- Every field has an associated `<label>` (use `for=`).
- Required fields use `aria-required="true"` not just a visual marker.
- Toggle uses `role="switch"` + `aria-checked`.
- Validation errors get `aria-invalid="true"` and an `aria-describedby` pointing to the message.

## Anti-patterns

- ❌ Floating labels (label inside the input).
- ❌ Checkboxes — use toggles. Mote has no checkbox component.
- ❌ Sliders with bubble values — show the value as a number, not floating above a thumb.
- ❌ Date pickers with calendar popups in chrome — Mote's audience prefers ISO dates typed in directly.
