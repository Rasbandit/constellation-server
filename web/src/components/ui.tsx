import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { fetchServerVersion, versionBadge, webVersion } from '../api/version';
import type { HintStatus, ItemFlag, RoomState, SlotStatus } from '../api/types';
import { flagLabel, hintStatusLabel, roomStateLabel, slotStatusLabel } from '../lib/format';

export function StatusPill({ status }: { status: SlotStatus }) {
  return (
    <span className={`pill pill-${status}`}>
      <span className="dot" aria-hidden />
      {slotStatusLabel[status]}
    </span>
  );
}

export function RoomStatePill({ state }: { state: RoomState }) {
  return <span className={`pill room-${state}`}>{roomStateLabel[state]}</span>;
}

export function FlagTag({ flag }: { flag: ItemFlag }) {
  return <span className={`flag flag-${flag}`}>{flagLabel[flag]}</span>;
}

export function HintTag({ status }: { status: HintStatus }) {
  return <span className={`hint-tag hint-${status}`}>{hintStatusLabel[status]}</span>;
}

export function Progress({ done, total, label }: { done: number; total: number; label?: string }) {
  const p = total === 0 ? 0 : Math.min(100, (done / total) * 100);
  return (
    <div className="progress" role="progressbar" aria-valuenow={done} aria-valuemin={0} aria-valuemax={total} aria-label={label}>
      <div className="progress-bar" style={{ width: `${p}%` }} />
      <span className="progress-text">
        {done}/{total}
      </span>
    </div>
  );
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="btn btn-small"
      onClick={() => {
        navigator.clipboard?.writeText(text).then(
          () => {
            setDone(true);
            setTimeout(() => setDone(false), 1500);
          },
          () => undefined,
        );
      }}
    >
      {done ? 'Copied' : label}
    </button>
  );
}

export function Card({ title, actions, children, className = '' }: { title?: ReactNode; actions?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`card ${className}`}>
      {(title || actions) && (
        <header className="card-head">
          {title && <h2>{title}</h2>}
          {actions && <div className="card-actions">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="empty">{children}</p>;
}

/** A drop target that also opens the file picker on click or Enter. */
export function Dropzone({ accept, onFiles, title, hint }: { accept: string; onFiles: (f: File[]) => void; title: string; hint: string }) {
  const input = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);
  return (
    <div
      className={`dropzone ${over ? 'over' : ''}`}
      role="button"
      tabIndex={0}
      onClick={() => input.current?.click()}
      onKeyDown={(e) => (e.key === 'Enter' || e.key === ' ') && input.current?.click()}
      onDragOver={(e) => {
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={() => setOver(false)}
      onDrop={(e) => {
        e.preventDefault();
        setOver(false);
        onFiles([...e.dataTransfer.files]);
      }}
    >
      <strong>{title}</strong>
      <span>{hint}</span>
      <input
        ref={input}
        type="file"
        multiple
        accept={accept}
        hidden
        onChange={(e) => {
          onFiles([...(e.target.files ?? [])]);
          e.target.value = '';
        }}
      />
    </div>
  );
}

export function Modal({ open, title, children, onClose }: { open: boolean; title: string; children: ReactNode; onClose: () => void }) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog ref={ref} className="modal" onClose={onClose} onCancel={onClose}>
      <header className="modal-head">
        <h2>{title}</h2>
        <button type="button" className="btn btn-ghost" aria-label="Close" onClick={onClose}>
          ✕
        </button>
      </header>
      {open && children}
    </dialog>
  );
}

// ---- Toasts --------------------------------------------------------------------------------

interface Toast {
  id: number;
  text: string;
  tone: 'ok' | 'error';
}
const ToastCtx = createContext<(text: string, tone?: Toast['tone']) => void>(() => undefined);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const show = useCallback((text: string, tone: Toast['tone'] = 'ok') => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, text, tone }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3500);
  }, []);
  return (
    <ToastCtx.Provider value={show}>
      {children}
      <div className="toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`toast toast-${t.tone}`}>
            {t.text}
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}

export const useToast = () => useContext(ToastCtx);

/** Permanent corner badge: server and web versions, flagged when they differ or the server is unreachable. */
export function VersionTag() {
  const [server, setServer] = useState<string>();
  useEffect(() => {
    void fetchServerVersion().then(setServer);
  }, []);
  const { text, mismatch } = versionBadge(server, webVersion);
  return (
    <small className={mismatch ? 'version-tag mismatch' : 'version-tag'} title={mismatch ? 'Server and web versions differ' : undefined}>
      {text}
    </small>
  );
}
