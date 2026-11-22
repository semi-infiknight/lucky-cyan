export function HardStopBlackoutButton({ enabled, run }: { enabled: boolean; run: () => void }) {
  if (!enabled) return null;
  return <button className="desk-hardstop desk-combined-stop" onClick={run}
    title="Hard-stop cues in all lists and hold DMX at zero until Restore lights">
    Hard stop + blackout
  </button>;
}

export function BlackoutStatus({ blackout, restore }: { blackout: boolean | null; restore: () => void }) {
  if (blackout === false) return null;
  return <div className="desk-blackout-status">
    <strong role="status">{blackout ? "DMX blackout active" : "DMX state unavailable"}</strong>
    {blackout && <button onClick={restore} title="Release blackout and output the current tracked DMX levels">Restore lights</button>}
  </div>;
}
