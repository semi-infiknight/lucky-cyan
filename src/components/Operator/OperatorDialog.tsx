import { useEffect, useRef, type ReactNode } from "react";
import "./operator.css";

export function OperatorDialog({ title, onClose, children }: {
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    return () => dialog?.close();
  }, []);

  return (
    <dialog ref={ref} className="operator-dialog" aria-label={title}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
      onKeyDown={(event) => event.stopPropagation()}>
      <header><h2>{title}</h2><button onClick={onClose} aria-label="Close dialog">Close</button></header>
      {children}
    </dialog>
  );
}

export function OperatorGuide({ onClose, onImport, onRehearse }: {
  onClose: () => void;
  onImport: () => void;
  onRehearse: () => void;
}) {
  return (
    <OperatorDialog title="Moving a show from QLab" onClose={onClose}>
      <section>
        <h3>Bring your workspace</h3>
        <p>Open a QLab 4 or 5 workspace or a QLab 3.0.14 .cues file with its media alongside it. Review the conversion report,
          relink missing files in Check Workspace, then use Collect and Save to make a portable copy.</p>
        <button className="primary" onClick={onImport}>Import QLab workspace…</button>
      </section>
      <section>
        <h3>Run a two-GO sound check</h3>
        <p>The sample opens at cue 1. Press Space to start a quiet tone. Press Space again to fade
          it over two seconds. Cue 3 follows and sends an OSC marker to this computer.
          The Sound checks cart holds an independent one-shot.</p>
        <button onClick={onRehearse}>Open rehearsal show</button>
      </section>
      <section>
        <h3>At the desk</h3>
        <dl><dt>Help for the selected cue in programming mode</dt><dd><kbd>F1</kbd></dd>
          <dt>GO at the playhead</dt><dd><kbd>Space</kbd></dd>
          <dt>Go to a cue number</dt><dd><kbd>G</kbd></dd>
          <dt>Stop all / hard stop all</dt><dd><kbd>Esc</kbd> / <kbd>Esc</kbd> twice</dd>
          <dt>Enter or leave Show Mode</dt><dd><kbd>F5</kbd></dd></dl>
      </section>
    </OperatorDialog>
  );
}
