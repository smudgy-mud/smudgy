import { createAlias, send } from "smudgy:core";

createAlias(/^imported$/, () => send("from-imported-package"));
