// Zustand store: workspace data, cue list, selection, and playhead.

import { create } from "zustand";
import type { CueId, CueListSummary, CueSummary, CueValidation, DisplayPreferences, GeneralPreferences, HealthAlert, VideoOutputInfo, WorkspaceInfo } from "../lib/types";
import { DEFAULT_DISPLAY_PREFS, DEFAULT_GENERAL_PREFS } from "../lib/types";
import { checkWorkspace, getAllCues, getCueLists, getHealthAlerts, getPlayhead, getPreferences, getWorkspaceInfo, listVideoOutputs } from "../lib/commands";

interface WorkspaceState {
  historyNotice: string | null;
  cues: CueSummary[];
  cueLists: CueListSummary[];
  activeCueListId: string | null;
  selectedCueId: CueId | null;
  /** All cues currently highlighted (multi-selection). Always includes selectedCueId when non-null. */
  selectedCueIds: CueId[];
  playheadCueId: CueId | null;
  workspaceInfo: WorkspaceInfo | null;
  generalPrefs: GeneralPreferences;
  displayPrefs: DisplayPreferences;
  /** Latest preflight results (one entry per cue with problems). */
  validation: CueValidation[];
  validationPending: boolean;
  validationError: string | null;
  /** IDs of cues with at least one error-severity problem (drives the row badge). */
  brokenCueIds: Set<CueId>;
  /** Active runtime health alerts (device/network faults) shown in the banner. */
  healthAlerts: HealthAlert[];
  /** Video outputs of the show: the main output first, then the extras. */
  videoOutputs: VideoOutputInfo[];

  // Actions
  refreshCues: () => Promise<void>;
  refreshValidation: () => Promise<void>;
  refreshHealth: () => Promise<void>;
  refreshVideoOutputs: () => Promise<void>;
  refreshCueLists: () => Promise<void>;
  setCueLists: (lists: CueListSummary[], activeId: string) => void;
  refreshWorkspaceInfo: () => Promise<void>;
  setSelectedCueId: (id: CueId | null) => void;
  setSelectedCueIds: (ids: CueId[]) => void;
  setPlayheadCueId: (id: CueId | null) => void;
  updateCueState: (cueId: CueId, state: CueSummary["state"]) => void;
  loadGeneralPrefs: () => Promise<void>;
  setGeneralPrefs: (p: GeneralPreferences) => void;
  loadDisplayPrefs: () => Promise<void>;
  setDisplayPrefs: (p: DisplayPreferences) => void;
}

// Bound background scans to one worker per open document, with one trailing read.
// Switching documents can start a new read even when the old venue share is offline.
let activeCheck: { instance: string | undefined; again: boolean; done: Promise<void> } | null = null;

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
  historyNotice: null,
  cues: [],
  cueLists: [],
  activeCueListId: null,
  selectedCueId: null,
  selectedCueIds: [],
  playheadCueId: null,
  workspaceInfo: null,
  validation: [],
  validationPending: true,
  validationError: null,
  brokenCueIds: new Set<CueId>(),
  healthAlerts: [],
  videoOutputs: [],
  generalPrefs: DEFAULT_GENERAL_PREFS,
  displayPrefs: { ...DEFAULT_DISPLAY_PREFS, output_screen: null, show_output_timer: false, timer_floating: false, timer_count_down: false, timer_font: "DSEG7 Classic", timer_font_size: 120, timer_position: "center" as const, timer_show_ms: false, timer_margin: 50, theme: "system" as const },

  refreshCues: async () => {
    try {
      const cues = await getAllCues();
      const playheadCueId = await getPlayhead();
      set({ cues, playheadCueId });
    } catch (e) {
      console.error("Failed to refresh cues:", e);
    }
  },

  refreshValidation: () => {
    const instance = get().workspaceInfo?.instance_id;
    if (activeCheck && activeCheck.instance === instance) {
      activeCheck.again = true;
      return activeCheck.done;
    }
    const request = { instance, again: false, done: Promise.resolve() };
    activeCheck = request;
    set({ validationPending: true, validationError: null });
    request.done = (async () => {
      do {
        request.again = false;
        try {
          const validation = await checkWorkspace();
          if (activeCheck !== request) return;
          if (request.again) continue;
          const brokenCueIds = new Set(validation.filter(v => v.issues.some(i => i.severity === "error")).map(v => v.cue_id));
          set({ validation, brokenCueIds, validationPending: false, validationError: null });
        } catch (error) {
          if (activeCheck !== request) return;
          if (request.again) continue;
          set({ validation: [], brokenCueIds: new Set(), validationPending: false, validationError: String(error) });
        }
      } while (request.again);
      if (activeCheck === request) activeCheck = null;
    })();
    return request.done;
  },

  refreshVideoOutputs: async () => {
    try {
      set({ videoOutputs: await listVideoOutputs() });
    } catch (e) {
      console.error("Failed to list video outputs:", e);
    }
  },

  refreshHealth: async () => {
    try {
      set({ healthAlerts: await getHealthAlerts() });
    } catch (e) {
      console.error("Failed to fetch health alerts:", e);
    }
  },

  refreshCueLists: async () => {
    try {
      const cueLists = await getCueLists();
      set((prev) => {
        // Keep the active ID only if it still exists in the new list.
        const validId = cueLists.some((cl) => cl.id === prev.activeCueListId)
          ? prev.activeCueListId
          : (cueLists[0]?.id ?? null);
        return { cueLists, activeCueListId: validId };
      });
    } catch (e) {
      console.error("Failed to refresh cue lists:", e);
    }
  },

  setCueLists: (lists, activeId) => set({ cueLists: lists, activeCueListId: activeId }),

  refreshWorkspaceInfo: async () => {
    try {
      const info = await getWorkspaceInfo();
      const previous = get().workspaceInfo;
      if (previous && (previous.instance_id !== info.instance_id || previous.revision !== info.revision || previous.file_path !== info.file_path)) {
        if (activeCheck?.instance === info.instance_id) activeCheck.again = true;
        else activeCheck = null;
        set({ workspaceInfo: info, validation: [], brokenCueIds: new Set(), validationPending: true, validationError: null });
      } else {
        set({ workspaceInfo: info });
      }
    } catch (e) {
      console.error("Failed to refresh workspace info:", e);
    }
  },

  setSelectedCueId: (id) => set({ selectedCueId: id }),

  setSelectedCueIds: (ids) => set({ selectedCueIds: ids }),

  setPlayheadCueId: (id) => set({ playheadCueId: id }),

  updateCueState: (cueId, state) => {
    set((prev) => ({
      cues: prev.cues.map((c) => (c.id === cueId ? { ...c, state } : c)),
    }));
  },

  loadGeneralPrefs: async () => {
    try {
      const prefs = await getPreferences();
      set({ generalPrefs: { ...DEFAULT_GENERAL_PREFS, ...prefs.general } });
    } catch (e) {
      console.error("Failed to load general preferences:", e);
    }
  },

  setGeneralPrefs: (p) => set({ generalPrefs: p }),

  loadDisplayPrefs: async () => {
    try {
      const prefs = await getPreferences();
      set({ displayPrefs: { ...DEFAULT_DISPLAY_PREFS, ...prefs.display } });
    } catch (e) {
      console.error("Failed to load display preferences:", e);
    }
  },

  setDisplayPrefs: (p) => set({ displayPrefs: p }),
}));
