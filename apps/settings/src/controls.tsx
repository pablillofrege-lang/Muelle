import { useId, useState, type ReactNode } from "react";

interface CardProps {
  /** Heading above the group, as macOS System Settings labels its groups. */
  title?: string;
  children: ReactNode;
}

export function Card({ title, children }: CardProps) {
  if (!title) return <section className="card">{children}</section>;
  return (
    <section className="card-group" aria-label={title}>
      <h2 className="card-title">{title}</h2>
      <div className="card">{children}</div>
    </section>
  );
}

interface RowProps {
  label: string;
  controlId?: string;
  hint?: string;
  icon?: ReactNode;
  stacked?: boolean;
  children: ReactNode;
}

export function Row({ label, controlId, hint, icon, stacked, children }: RowProps) {
  return (
    <div className={stacked ? "row row-stacked" : "row"}>
      {icon ? <span className="row-icon">{icon}</span> : null}
      <div className="row-text">
        {controlId ? <label className="row-label" htmlFor={controlId}>{label}</label>
          : <span className="row-label">{label}</span>}
        {hint ? <span className="row-hint">{hint}</span> : null}
      </div>
      <div className="row-control">{children}</div>
    </div>
  );
}

interface SubRowProps extends Omit<RowProps, "icon"> {
  /** Whether the option it depends on is on; the row folds away while it is off. */
  open: boolean;
}

/**
 * An option that only makes sense while the row above it is on. It stays
 * mounted so it can fold open and shut, and is `inert` while folded so the
 * keyboard and screen readers skip it.
 */
export function SubRow({ open, ...row }: SubRowProps) {
  return (
    <div className="row-sub" data-open={open} inert={!open}>
      <div className="row-sub-clip">
        <Row {...row} />
      </div>
    </div>
  );
}

interface ToggleProps {
  /** Undefined while an inherited system value is still being read. */
  checked: boolean | undefined;
  label: string;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}

export function Toggle({ checked, label, disabled = false, onChange }: ToggleProps) {
  const pending = checked === undefined;
  return (
    <button
      type="button"
      role={pending ? undefined : "switch"}
      aria-checked={checked}
      aria-label={label}
      aria-busy={pending || undefined}
      disabled={pending || disabled}
      className="toggle"
      onClick={() => { if (!pending && !disabled) onChange(!checked); }}
    >
      {pending ? <span className="toggle-pending" aria-hidden="true">…</span> : <span className="toggle-knob" />}
    </button>
  );
}

interface SliderProps {
  value: number;
  min: number;
  max: number;
  label: string;
  unit?: string;
  disabled?: boolean;
  onChange: (value: number) => void;
}

export function Slider({ value, min, max, label, unit = "px", disabled, onChange }: SliderProps) {
  const filled = ((value - min) / (max - min)) * 100;
  return (
    <div className="slider">
      <input
        type="range"
        aria-label={label}
        min={min}
        max={max}
        value={value}
        aria-valuetext={`${value} ${unit}`}
        disabled={disabled}
        style={{ "--filled": `${filled}%` } as React.CSSProperties}
        onChange={(event) => onChange(Number(event.target.value))}
      />
      <output className="slider-value" aria-hidden="true">{value} {unit}</output>
    </div>
  );
}

interface SelectProps<T extends string> {
  value: T;
  label: string;
  id?: string;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
}

interface PreferenceInputProps {
  label: string;
  value: string | number;
  min?: number;
  max?: number;
  maxLength?: number;
  invalidText?: string;
  onChange: (value: string) => void;
}

/** Commits once the user leaves the field or presses Enter. */
export function PreferenceInput({ label, value, min, max, maxLength, invalidText, onChange }: PreferenceInputProps) {
  const errorId = useId();
  const [draft, setDraft] = useState<string | null>(null);
  const [invalid, setInvalid] = useState(false);
  const number = typeof value === "number";
  return (
    <div className="preference-field">
      <input
        type={number ? "number" : "text"}
        inputMode={number ? "decimal" : undefined}
        step={number ? "any" : undefined}
        required={number}
        min={min}
        max={max}
        maxLength={maxLength}
        aria-label={label}
        aria-invalid={invalid || undefined}
        aria-describedby={invalid ? errorId : undefined}
        value={draft ?? value}
        onChange={(event) => {
          const input = event.currentTarget;
          setDraft(input.value);
          if (input.validity.valid) setInvalid(false);
        }}
        onBlur={(event) => {
          if (draft === null) return;
          const input = event.currentTarget;
          if (!input.validity.valid) {
            setInvalid(true);
            return;
          }
          const next = input.value.trim();
          if (next !== String(value)) onChange(next);
          setDraft(null);
          setInvalid(false);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.nativeEvent.isComposing) event.currentTarget.blur();
        }}
      />
      {invalid && <span id={errorId} className="row-hint" role="status">{invalidText}</span>}
    </div>
  );
}

/** Native popup and keyboard navigation, styled like the other Settings controls. */
export function Select<T extends string>({ value, label, id, options, onChange }: SelectProps<T>) {
  return <span className="select-wrap">
    <select className="select-control" id={id} aria-label={id ? undefined : label} value={value}
      onChange={(event) => onChange(event.target.value as T)}>
      {options.map((option) => <option key={option.value} value={option.value} dir="auto">{option.label}</option>)}
    </select>
    <svg className="select-chevron" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" aria-hidden="true">
      <path d="m4 6 4 4 4-4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  </span>;
}

interface VisualChoicesProps<T extends string> {
  label: string;
  value: T;
  hideLabel?: boolean;
  /** Wrap into rows of this many; one row by default. */
  columns?: number;
  options: readonly { value: T; label: string; preview: ReactNode; detail?: string }[];
  onChange: (value: T) => void;
}

/** Native radios provide arrow-key navigation without extra listeners or state. */
export function VisualChoices<T extends string>({ label, value, hideLabel, columns, options, onChange }: VisualChoicesProps<T>) {
  const name = useId();
  return (
    <fieldset className="visual-setting">
      <legend className={hideLabel ? "sr-only" : "visual-legend"}>{label}</legend>
      <div className="visual-choices" style={{ gridTemplateColumns: `repeat(${columns ?? options.length}, minmax(0, 1fr))` }}>
        {options.map((option) => (
          <label className="visual-choice" key={option.value}>
            <input type="radio" name={name} value={option.value} checked={value === option.value}
              onChange={() => onChange(option.value)} />
            <span className="visual-choice-body">
              <span className="visual-preview" aria-hidden="true">{option.preview}</span>
              <span className="visual-choice-label">{option.label}
                <span className="visual-check" aria-hidden="true">
                <svg viewBox="0 0 16 16" width="12" height="12"><path d="m4 8 2.5 2.5L12 5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" /></svg>
                </span>
              </span>
              {option.detail ? <span className="visual-choice-detail">{option.detail}</span> : null}
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}

interface SegmentedProps<T extends string> {
  value: T;
  label: string;
  options: readonly { value: T; label: string }[];
  onChange: (value: T) => void;
}

export function Segmented<T extends string>({ value, label, options, onChange }: SegmentedProps<T>) {
  const name = useId();
  return (
    <div className="segmented" role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <label key={option.value} className="segment">
          <input type="radio" className="sr-only" name={name} value={option.value}
            checked={option.value === value} onChange={() => onChange(option.value)} />
          <span>{option.label}</span>
        </label>
      ))}
    </div>
  );
}
