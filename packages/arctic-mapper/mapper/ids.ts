/** Compare canonical mapper UUID strings, including absent destinations. */
export function idsMatch(
    id1: AreaId | undefined | null,
    id2: AreaId | undefined | null,
): boolean {
    if (!id1 && !id2) {
        return true;
    }

    if (!id1 || !id2) {
        return false;
    }

    return id1 === id2;
}
