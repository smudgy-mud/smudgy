export const NUKEFIRE_ATLAS_NAME = "Nukefire";

/** Find the NukeFire atlas in local storage, creating it when absent. */
export async function upsertLocalNukeFireAtlas(
  atlasMapper: Pick<Mapper, "listAtlases" | "createAtlas">,
): Promise<Atlas> {
  const existing = (await atlasMapper.listAtlases()).find((atlas) =>
    atlas.storage === "local" && atlas.name === NUKEFIRE_ATLAS_NAME
  );
  return existing ?? await atlasMapper.createAtlas(NUKEFIRE_ATLAS_NAME, {
    storage: "local",
  });
}
