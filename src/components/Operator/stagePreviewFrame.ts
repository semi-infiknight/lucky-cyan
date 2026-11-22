const HEADER_BYTES = 20;

export interface StagePreviewFrame {
  serial: number;
  width: number;
  height: number;
  sourceWidth: number;
  sourceHeight: number;
  pixels: Uint8ClampedArray<ArrayBuffer>;
}

export function decodeStagePreview(reply: ArrayBuffer | number[]): StagePreviewFrame | null {
  // Tauri's protocol sends a binary ArrayBuffer. Its message fallback uses
  // an array of bytes, so use the same packet decoder on either route.
  const buffer = reply instanceof ArrayBuffer ? reply : Uint8Array.from(reply).buffer;
  if (buffer.byteLength === 0) return null;
  if (buffer.byteLength < HEADER_BYTES) throw new Error("Incomplete stage preview frame.");
  const header = new DataView(buffer);
  const serial = header.getUint32(0, true);
  const width = header.getUint32(4, true);
  const height = header.getUint32(8, true);
  const sourceWidth = header.getUint32(12, true);
  const sourceHeight = header.getUint32(16, true);
  if (!serial || !width || !height || width > 320 || height > 180
      || sourceWidth < width || sourceHeight < height
      || buffer.byteLength !== HEADER_BYTES + width * height * 4) {
    throw new Error("Invalid stage preview frame.");
  }
  return {
    serial, width, height, sourceWidth, sourceHeight,
    pixels: new Uint8ClampedArray(buffer, HEADER_BYTES),
  };
}
