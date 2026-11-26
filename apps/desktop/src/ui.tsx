import {
  useEffect, useId, useLayoutEffect, useRef, useState,
  type ButtonHTMLAttributes, type KeyboardEvent, type ReactNode, type RefObject,
} from "react";
import { AlertIcon, CloseIcon, InfoIcon } from "./icons";

export function IconButton({
  label, children, className = "", ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; children: ReactNode }) {
  return (
    <button type="button" className={`icon-button ${className}`} aria-label={label} title={label} {...props}>
      {children}
    </button>
  );
}

function useDismiss(open: boolean, close: () => void, root: RefObject<HTMLElement | null>) {
  useEffect(() => {
    if (!open) return;
    const onPointer = (event: PointerEvent) => {
      if (root.current && !root.current.contains(event.target as Node)) close();
    };
    document.addEventListener("pointerdown", onPointer);
    return () => document.removeEventListener("pointerdown", onPointer);
  }, [open, close, root]);
}

function focusItems(container: HTMLElement | null): HTMLElement[] {
  return container
    ? [...container.querySelectorAll<HTMLElement>("[role=menuitem]:not([disabled]),[role=menuitemradio]:not([disabled])")]
    : [];
}

/** An accessible menu button. Items should be MenuItem elements. */
export function Menu({
  label, trigger, children, align = "start", placement = "below", disabled = false, className = "",
}: {
  label: string;
  trigger: ReactNode;
  children: ReactNode | ((close: () => void) => ReactNode);
  align?: "start" | "end";
  placement?: "below" | "above";
  disabled?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const id = useId();
  const close = useRef(() => {
    setOpen(false);
  }).current;
  useDismiss(open, close, root);

  useEffect(() => {
    if (open) focusItems(list.current)[0]?.focus();
  }, [open]);

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === "Escape" && open) {
      event.stopPropagation();
      setOpen(false);
      button.current?.focus();
      return;
    }
    if (!open || !["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
    event.preventDefault();
    const items = focusItems(list.current);
    if (!items.length) return;
    const current = items.indexOf(document.activeElement as HTMLElement);
    const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1
      : event.key === "ArrowDown" ? (current + 1) % items.length : (current - 1 + items.length) % items.length;
    items[next].focus();
  }

  const closeAndFocus = () => {
    setOpen(false);
    button.current?.focus();
  };

  return (
    <div className={`menu-root ${className}`} ref={root} onKeyDown={onKeyDown}>
      <button ref={button} type="button" className="menu-trigger" aria-haspopup="menu" aria-expanded={open}
        aria-controls={open ? id : undefined} aria-label={label} title={label} disabled={disabled}
        onClick={() => setOpen((value) => !value)}>
        {trigger}
      </button>
      {open && (
        <div id={id} ref={list} role="menu" aria-label={label}
          className={`menu-list menu-${align} menu-${placement}`}>
          {typeof children === "function" ? children(closeAndFocus) : children}
        </div>
      )}
    </div>
  );
}

export function MenuItem({
  children, onSelect, disabled = false, danger = false, checked, icon, hint,
}: {
  children: ReactNode;
  onSelect: () => void;
  disabled?: boolean;
  danger?: boolean;
  checked?: boolean;
  icon?: ReactNode;
  hint?: ReactNode;
}) {
  return (
    <button type="button" role={checked === undefined ? "menuitem" : "menuitemradio"}
      aria-checked={checked} tabIndex={-1} disabled={disabled}
      className={`menu-item${danger ? " menu-item-danger" : ""}`} onClick={onSelect}>
      <span className="menu-item-icon">{icon}</span>
      <span className="menu-item-text">
        <span>{children}</span>
        {hint && <small>{hint}</small>}
      </span>
      {checked && <span className="menu-item-check" aria-hidden="true">✓</span>}
    </button>
  );
}

export function MenuSeparator() {
  return <div className="menu-separator" role="separator" />;
}

/** A non-modal popover panel anchored to a trigger button. */
export function Popover({
  label, trigger, children, align = "start", placement = "above", disabled = false, triggerClassName = "", panelClassName = "", fitViewport = false,
}: {
  panelClassName?: string;
  fitViewport?: boolean;
  label: string;
  trigger: ReactNode;
  children: ReactNode;
  align?: "start" | "end";
  placement?: "below" | "above";
  disabled?: boolean;
  triggerClassName?: string;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const id = useId();
  const close = useRef(() => setOpen(false)).current;
  useDismiss(open, close, root);

  useLayoutEffect(() => {
    const node = panel.current;
    if (!open || !fitViewport || !node) return;
    const fit = () => {
      node.style.translate = "";
      const bounds = node.getBoundingClientRect();
      const left = Math.max(12, Math.min(bounds.left, document.documentElement.clientWidth - bounds.width - 12));
      node.style.translate = `${left - bounds.left}px`;
    };
    fit();
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  }, [open, fitViewport]);

  useEffect(() => {
    if (open) panel.current?.querySelector<HTMLElement>("input:enabled,select:enabled,button:enabled,textarea:enabled")?.focus();
  }, [open]);

  // A control that disables itself (such as "All" once everything is
  // selected) drops focus to the page, outside this root. Escape still closes.
  useEffect(() => {
    if (!open) return;
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      const target = event.target instanceof Node ? event.target : null;
      if (target && root.current?.contains(target)) return;
      setOpen(false);
      button.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open]);

  return (
    <div className="menu-root" ref={root} onKeyDown={(event) => {
      if (event.key === "Escape" && open) {
        event.stopPropagation();
        setOpen(false);
        button.current?.focus();
      }
    }}>
      <button ref={button} type="button" className={`menu-trigger ${triggerClassName}`} aria-expanded={open}
        aria-controls={open ? id : undefined} aria-label={label} title={label} disabled={disabled}
        onClick={() => setOpen((value) => !value)}>
        {trigger}
      </button>
      {open && (
        <div id={id} ref={panel} role="dialog" aria-label={label} className={`popover menu-${align} menu-${placement} ${panelClassName}`}>
          {children}
        </div>
      )}
    </div>
  );
}

export function Dialog({
  title, description, children, onClose, busy = false, size = "md",
}: {
  title: string;
  description?: ReactNode;
  children: ReactNode;
  onClose: () => void;
  busy?: boolean;
  size?: "sm" | "md" | "lg" | "xl";
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const descriptionId = useId();

  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const previous = document.activeElement;
    dialog.showModal();
    // Start focus on the dialog itself unless a field asks for it, so the close
    // button isn't highlighted on open. Tab then moves through the controls.
    // React's autoFocus renders no attribute, so fields opt in with data-autofocus.
    const initial = dialog.querySelector<HTMLElement>("[data-autofocus]");
    if (initial) initial.focus();
    else dialog.focus();
    return () => {
      dialog.close();
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);

  return (
    <dialog ref={ref} className={`dialog dialog-${size}`} aria-labelledby={titleId} tabIndex={-1}
      aria-describedby={description ? descriptionId : undefined}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) onClose();
      }}>
      <header className="dialog-heading">
        <div>
          <h2 id={titleId}>{title}</h2>
          {description && <p id={descriptionId} className="dialog-description">{description}</p>}
        </div>
        <IconButton label="Close" onClick={onClose} disabled={busy}><CloseIcon /></IconButton>
      </header>
      <div className="dialog-body">{children}</div>
    </dialog>
  );
}

export function Switch({
  checked, onChange, label, description, disabled = false,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  label: string;
  description?: string;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="switch-row">
      <div>
        <label htmlFor={id} className="switch-label">{label}</label>
        {description && <p className="hint">{description}</p>}
      </div>
      <button id={id} type="button" role="switch" aria-checked={checked} disabled={disabled}
        className="switch" onClick={() => onChange(!checked)}>
        <span className="switch-thumb" />
      </button>
    </div>
  );
}

export function Segmented<T extends string>({
  value, options, onChange, label, disabled = false,
}: {
  value: T;
  options: { value: T; label: string; icon?: ReactNode }[];
  onChange: (value: T) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <div className="segmented" role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <button key={option.value} type="button" role="radio" aria-checked={value === option.value}
          disabled={disabled} onClick={() => onChange(option.value)}>
          {option.icon}{option.label}
        </button>
      ))}
    </div>
  );
}

export function Callout({
  tone = "neutral", title, children, actions, role,
}: {
  tone?: "neutral" | "warning" | "error" | "accent";
  title?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  role?: "alert" | "status";
}) {
  return (
    <div className={`callout callout-${tone}`} role={role}>
      <span className="callout-icon">{tone === "error" || tone === "warning" ? <AlertIcon /> : <InfoIcon />}</span>
      <div className="callout-body">
        {title && <strong>{title}</strong>}
        {children && <div className="callout-text">{children}</div>}
        {actions && <div className="callout-actions">{actions}</div>}
      </div>
    </div>
  );
}
