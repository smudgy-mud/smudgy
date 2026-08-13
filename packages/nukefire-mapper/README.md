# NukeFire Mapper

`smudgy://kapusniak/nukefire-mapper` reads NukeFire's GMCP data and
automatically builds the map shown in Smudgy's map widget.

As you explore, it adds rooms, exits, terrain, doors, and vertical connections.
A room you reach by going up or down is always mapped one level above or below
the room you left, even when the game charts both on the same plane. It also
follows your current location and displays GPS routes. New maps are
saved locally, survive restarts, and are not synced to the cloud; a NukeFire
map you already keep in the cloud is adopted and stays there rather than being
duplicated into a local copy.

## Setup

Enable the package and connect to NukeFire; mapping starts automatically. For
the complete map and radar interface, also enable
`smudgy://kapusniak/nukefire-scripts`.

Disable `smudgy://official/auto-mapper` while using this package. Running both
mappers can cause conflicting room and exit updates.
