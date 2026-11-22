import type { FixtureParam, FixtureType, PatchedChannel, PatchedFixture } from "./types";

export function fixtureParameterAddress(fixture: PatchedFixture, param: FixtureParam): PatchedChannel {
  const base = fixture.base_address - 1;
  return {
    universe: fixture.universe,
    channel: base + param.channel_offset,
    width: param.width,
    ...(param.fine_channel_offset === undefined ? {} : { fine_channel: base + param.fine_channel_offset }),
  };
}

/** Both slots are zero-based within the universe. */
export function channelSlots(address: PatchedChannel): number[] {
  return address.width === "Bit16"
    ? [address.channel, address.fine_channel ?? address.channel + 1]
    : [address.channel];
}

export function fixtureChannelFits(address: PatchedChannel): boolean {
  const slots = channelSlots(address);
  return (address.width === "Bit16" || address.fine_channel === undefined)
    && slots[0] !== slots[1]
    && slots.every(slot => Number.isInteger(slot) && slot >= 0 && slot < 512);
}

export function fixtureFootprint(profile: FixtureType): number {
  return profile.parameters.reduce((end, param) => Math.max(end, ...channelSlots({
    universe: 0, channel: param.channel_offset, width: param.width, fine_channel: param.fine_channel_offset,
  }).map(slot => slot + 1)), 0);
}
