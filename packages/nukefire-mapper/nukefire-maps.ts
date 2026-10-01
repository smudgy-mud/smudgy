/**
 * What is particular to NukeFire about its maps: the area names the package
 * knows, and the properties and names that mark the maps and rooms the mapper
 * made. The modules that settle zones into maps take both from here.
 */

import type { AreaNameRule } from "./area-names.ts";
import type { ZoneMarks } from "./zone-settle.ts";

const anyCase = (name: string, map: string): AreaNameRule => ({ name, map, exactCase: false });

/** What the package knows about NukeFire's area names. It seeds the settings table, which the player owns. */
export const NUKEFIRE_AREA_NAME_RULES: readonly AreaNameRule[] = [
  anyCase("Vega Jane II", "Vega Jane"),
  anyCase("Vega Jane III", "Vega Jane"),
  anyCase("Vega Jane IV", "Vega Jane"),
  anyCase("Dread Dungeon 253", "Dread Dungeon"),
  anyCase("Dread Dungeon 254", "Dread Dungeon"),
  anyCase("Dread Dungeon 258", "Dread Dungeon"),
  anyCase("Monster Island II", "Monster Island"),
  anyCase("Shadowspire II", "Shadowspire"),
  anyCase("SST - Federation Station", "SST"),
  anyCase("Jurassic Park II", "Jurassic Park"),
  anyCase("Jurassic World", "Jurassic Park"),
  { name: "DARK PLEASURES", map: "Darker Pleasures", exactCase: true },
];

const PLACEHOLDER = /^NukeFire Zone (\d+)$/;

export const NUKEFIRE_MARKS: ZoneMarks = {
  areaKey: "nukefire.area",
  managed: { property: "nukefire.mapper", value: "NukeFire.Map.Local" },
  zone: "nukefire.zone",
  placeholderName: (zone) => `NukeFire Zone ${zone}`,
  placeholderZone: (name) => {
    const zone = PLACEHOLDER.exec(name)?.[1];
    return zone === undefined ? undefined : Number(zone);
  },
};
