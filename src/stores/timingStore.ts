// Per-cue timing data updated at ~30 fps from cue-time-update events.

import { create } from "zustand";
import type { CueId } from "../lib/types";

export interface CueTiming {
  workspace_id?: string;
  elapsed_ms: number;
  action_elapsed_ms: number;
  remaining_ms: number | null;
}

interface TimingState {
  timings: Record<CueId, CueTiming>;
  setTiming: (cueId: CueId, t: CueTiming) => void;
  clearTiming: (cueId: CueId) => void;
}

export const useTimingStore = create<TimingState>((set) => ({
  timings: {},

  setTiming: (cueId, t) =>
    set((s) => ({ timings: { ...s.timings, [cueId]: t } })),

  clearTiming: (cueId) =>
    set((s) => {
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      const { [cueId]: _removed, ...rest } = s.timings;
      return { timings: rest };
    }),
}));
