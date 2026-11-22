import type { CueType } from "../../lib/types";

export const manualHome = "operator-guide/README.md";

const topics: Partial<Record<CueType, string>> = {
  audio: "audio", mic: "audio", fade: "audio", devamp: "audio",
  video: "video", image: "video", camera: "video", text: "video",
  midi: "midi", midi_file: "midi", timecode: "workshops", osc: "workshops",
  light: "lighting", script: "automation",
};

export function manualChapterFor(cueType?: CueType) {
  if (!cueType) return manualHome;
  return `operator-guide/${topics[cueType] ?? "programming"}/README.md`;
}
