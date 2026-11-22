export const CHANNEL_LIMIT = 128;
export const SILENT_DB = -60;
export const MAX_DB = 12;

/** Only the edited row is copied. An empty row carries no routes. */
export function withCrosspoint(matrix: number[][], input: number, output: number, db: number): number[][] {
  if (!Number.isInteger(input) || !Number.isInteger(output) || input < 0 || output < 0 ||
      input >= CHANNEL_LIMIT || output >= CHANNEL_LIMIT || !Number.isFinite(db)) {
    return matrix;
  }
  const level = Math.min(MAX_DB, Math.max(SILENT_DB, db));
  if ((matrix[input]?.[output] ?? SILENT_DB) === level) return matrix;
  const next = matrix.slice();
  while (next.length <= input) next.push([]);
  const row = next[input].slice();
  while (row.length <= output) row.push(SILENT_DB);
  row[output] = level;
  next[input] = row;
  return next;
}

export function parseCrosspoint(text: string): number | undefined {
  const value = text.trim().replace(/^−/, "-");
  if (value === "-∞" || /^-inf(?:inity)?$/i.test(value)) return SILENT_DB;
  if (!/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)$/.test(value)) return undefined;
  const number = Number(value);
  return Number.isFinite(number) ? Math.min(MAX_DB, Math.max(SILENT_DB, number)) : undefined;
}

export function stereoMatrix(): number[][] {
  return [[0, SILENT_DB], [SILENT_DB, 0]];
}

export const INPUT_BANK_SIZE = 8;
export const OUTPUT_BANK_SIZE = 4;

export interface RoutedBank {
  inputStart: number;
  outputStart: number;
  count: number;
}

/** Index active banks so the operator can find an imported route without opening every page. */
export function routedBanks(matrix: number[][] | null): RoutedBank[] {
  if (!matrix) return [];
  const banks = new Map<number, RoutedBank>();
  const columns = CHANNEL_LIMIT / OUTPUT_BANK_SIZE;
  matrix.slice(0, CHANNEL_LIMIT).forEach((row, input) => {
    row.slice(0, CHANNEL_LIMIT).forEach((db, output) => {
      if (!Number.isFinite(db) || db <= SILENT_DB) return;
      const inputBank = Math.floor(input / INPUT_BANK_SIZE);
      const outputBank = Math.floor(output / OUTPUT_BANK_SIZE);
      const key = inputBank * columns + outputBank;
      const bank = banks.get(key);
      if (bank) bank.count += 1;
      else banks.set(key, { inputStart: inputBank * INPUT_BANK_SIZE, outputStart: outputBank * OUTPUT_BANK_SIZE, count: 1 });
    });
  });
  return [...banks.values()].sort((a, b) => a.inputStart - b.inputStart || a.outputStart - b.outputStart);
}

/** Keep logical input numbers stable when earlier channels are unnamed. */
export function withSourceChannelName(names: readonly string[], input: number, name: string): string[] {
  const next = Array.from({ length: Math.max(names.length, input + 1) }, (_, i) => names[i] ?? "");
  next[input] = name.trim();
  while (next.length && !next[next.length - 1]) next.pop();
  return next;
}
