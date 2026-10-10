// Persistent in-memory TypeScript LanguageService used by Smudgy's authoring UI.
// The Rust host owns every file snapshot. This driver never reads the filesystem or
// reaches the network; TypeScript's synchronous host sees only the tables below.
(function () {
  const ts = globalThis.ts;
  if (!ts || typeof ts.createLanguageService !== "function") {
    throw new Error("embedded TypeScript LanguageService is unavailable");
  }

  let libraries = Object.freeze({});
  let libraryRoots = Object.freeze([]);
  let documents = Object.freeze({});
  let roots = Object.freeze([]);
  let projectVersion = 0;
  let initialized = false;
  let poisoned = false;
  let disposed = false;

  const compilerOptions = Object.freeze({
    strict: true,
    noEmit: true,
    target: ts.ScriptTarget.ESNext,
    module: ts.ModuleKind.ESNext,
    moduleResolution: ts.ModuleResolutionKind.Bundler,
    // Keep each authored body lexically isolated. Inline automations execute in
    // their own host-created scope and may legally shadow API globals such as `id`.
    moduleDetection: ts.ModuleDetectionKind.Force,
    allowJs: true,
    checkJs: true,
    allowImportingTsExtensions: true,
    resolveJsonModule: true,
    skipLibCheck: true,
    jsx: ts.JsxEmit.ReactJSX,
    jsxImportSource: "smudgy:widgets",
  });
  const MAX_SIGNATURE_HELP_PARAMETERS = 256;
  const MAX_SIGNATURE_HELP_COUNT = 0xffff;
  const MAX_INFERRED_VAR_PROPERTIES = 500;
  const MAX_INFERRED_VAR_TYPE_LENGTH = 2048;
  const INLINE_MODULE_SYNTAX_CODE = 97001;
  const INLINE_TOP_LEVEL_AWAIT_CODE = 97002;

  function normalize(fileName) {
    let value = String(fileName).replace(/\\/g, "/");
    if (!value.startsWith("/")) value = "/" + value;
    const parts = [];
    for (const part of value.split("/")) {
      if (!part || part === ".") continue;
      if (part === "..") parts.pop();
      else parts.push(part);
    }
    return "/" + parts.join("/");
  }

  function entry(fileName) {
    const name = normalize(fileName);
    return documents[name] || libraries[name];
  }

  function directoryOf(fileName) {
    const name = normalize(fileName);
    const slash = name.lastIndexOf("/");
    return slash <= 0 ? "/" : name.slice(0, slash);
  }

  function extensionFor(fileName) {
    const lower = fileName.toLowerCase();
    if (lower.endsWith(".d.mts")) return ts.Extension.Dmts;
    if (lower.endsWith(".d.cts")) return ts.Extension.Dcts;
    if (lower.endsWith(".d.ts")) return ts.Extension.Dts;
    if (lower.endsWith(".mts")) return ts.Extension.Mts;
    if (lower.endsWith(".cts")) return ts.Extension.Cts;
    if (lower.endsWith(".mjs")) return ts.Extension.Mjs;
    if (lower.endsWith(".cjs")) return ts.Extension.Cjs;
    if (lower.endsWith(".tsx")) return ts.Extension.Tsx;
    if (lower.endsWith(".jsx")) return ts.Extension.Jsx;
    if (lower.endsWith(".js")) return ts.Extension.Js;
    if (lower.endsWith(".json")) return ts.Extension.Json;
    return ts.Extension.Ts;
  }

  function exactRelativeTarget(moduleName, containingFile) {
    if (!moduleName.startsWith(".")) return undefined;
    const containingName = normalize(containingFile);
    const projectMatch = /^\/projects\/[^/]+\/[^/]+\//.exec(containingName);
    const target = normalize(directoryOf(containingFile) + "/" + moduleName);
    if (projectMatch && !target.startsWith(projectMatch[0])) return undefined;
    return entry(target) ? target : undefined;
  }

  const host = {
    getCompilationSettings() {
      return compilerOptions;
    },
    getScriptFileNames() {
      return roots.slice();
    },
    getScriptVersion(fileName) {
      const found = entry(fileName);
      return found ? String(found.version) : "0";
    },
    getScriptSnapshot(fileName) {
      const found = entry(fileName);
      return found ? ts.ScriptSnapshot.fromString(found.text) : undefined;
    },
    getProjectVersion() {
      return String(projectVersion);
    },
    getCurrentDirectory() {
      return "/";
    },
    getDefaultLibFileName() {
      return "/lib.d.ts";
    },
    useCaseSensitiveFileNames() {
      return true;
    },
    getNewLine() {
      return "\n";
    },
    fileExists(fileName) {
      return entry(fileName) !== undefined;
    },
    readFile(fileName) {
      const found = entry(fileName);
      return found && found.text;
    },
    directoryExists(directoryName) {
      const prefix = normalize(directoryName).replace(/\/$/, "") + "/";
      return Object.keys(documents).some((name) => name.startsWith(prefix)) ||
        Object.keys(libraries).some((name) => name.startsWith(prefix));
    },
    getDirectories(directoryName) {
      const prefix = normalize(directoryName).replace(/\/$/, "") + "/";
      const children = new Set();
      for (const name of Object.keys(documents).concat(Object.keys(libraries))) {
        if (!name.startsWith(prefix)) continue;
        const rest = name.slice(prefix.length);
        const slash = rest.indexOf("/");
        if (slash >= 0) children.add(prefix + rest.slice(0, slash));
      }
      return Array.from(children);
    },
    realpath(fileName) {
      return normalize(fileName);
    },
    resolveModuleNames(moduleNames, containingFile) {
      return moduleNames.map((moduleName) => {
        const fileName = exactRelativeTarget(moduleName, containingFile);
        return fileName ? {
          resolvedFileName: fileName,
          extension: extensionFor(fileName),
          isExternalLibraryImport: false,
        } : undefined;
      });
    },
    resolveTypeReferenceDirectives(typeDirectiveNames) {
      return typeDirectiveNames.map((directive) => {
        const name = typeof directive === "string" ? directive : directive.fileName;
        if (name !== "node") return undefined;
        const fileName = "/node-types/@types/node/ts5.6/index.d.ts";
        return entry(fileName) ? {
          resolvedFileName: fileName,
          primary: true,
        } : undefined;
      });
    },
  };

  const service = ts.createLanguageService(host, ts.createDocumentRegistry());

  function makeTable(files, previous) {
    const next = {};
    for (const [rawName, rawText] of Object.entries(files || {})) {
      const name = normalize(rawName);
      const text = String(rawText);
      const old = previous && previous[name];
      next[name] = Object.freeze({
        text,
        version: old && old.text === text ? old.version : old ? old.version + 1 : 1,
      });
    }
    return Object.freeze(next);
  }

  function inlineProjectRoot(table) {
    for (const name of Object.keys(table)) {
      const match = /^(\/projects\/[^/]+\/[^/]+\/)inline\/context\.d\.ts$/.exec(name);
      if (match) return match[1];
    }
    return undefined;
  }

  function inlineSourceFiles(program, projectRoot) {
    return program.getSourceFiles().filter((sourceFile) =>
      sourceFile.fileName.startsWith(projectRoot) &&
      documents[normalize(sourceFile.fileName)] &&
      !sourceFile.fileName.endsWith(".d.ts") &&
      !sourceFile.fileName.endsWith(".d.mts") &&
      !sourceFile.fileName.endsWith(".d.cts")
    );
  }

  function varsPropertyWrite(node) {
    if (!ts.isBinaryExpression(node)) return undefined;
    if (
      node.operatorToken.kind !== ts.SyntaxKind.EqualsToken &&
      node.operatorToken.kind !== ts.SyntaxKind.QuestionQuestionEqualsToken &&
      node.operatorToken.kind !== ts.SyntaxKind.BarBarEqualsToken &&
      node.operatorToken.kind !== ts.SyntaxKind.AmpersandAmpersandEqualsToken
    ) {
      return undefined;
    }
    const target = node.left;
    if (ts.isPropertyAccessExpression(target) && ts.isIdentifier(target.expression)) {
      return target.expression.text === "vars"
        ? { receiver: target.expression, name: target.name.text, value: node.right }
        : undefined;
    }
    if (
      ts.isElementAccessExpression(target) &&
      ts.isIdentifier(target.expression) &&
      target.expression.text === "vars" &&
      target.argumentExpression &&
      (ts.isStringLiteralLike(target.argumentExpression) ||
        ts.isNumericLiteral(target.argumentExpression))
    ) {
      return {
        receiver: target.expression,
        name: target.argumentExpression.text,
        value: node.right,
      };
    }
    return undefined;
  }

  function isInlineVarsSymbol(checker, receiver, projectRoot) {
    const symbol = checker.getSymbolAtLocation(receiver);
    return !!symbol && (symbol.declarations || []).some((declaration) =>
      normalize(declaration.getSourceFile().fileName) === projectRoot + "inline/context.d.ts"
    );
  }

  function projectLocalSymbol(symbol, projectRoot) {
    return !!symbol && (symbol.declarations || []).some((declaration) => {
      const fileName = normalize(declaration.getSourceFile().fileName);
      return fileName.startsWith(projectRoot) && !declaration.getSourceFile().isDeclarationFile;
    });
  }

  function portableTypeText(checker, type, location, projectRoot, seen, depth) {
    if (depth > 6 || seen.has(type)) return "unknown";
    if (type.isUnion && type.isUnion()) {
      return type.types.map((part) =>
        portableTypeText(checker, part, location, projectRoot, seen, depth + 1)
      ).join(" | ");
    }
    if (type.isIntersection && type.isIntersection()) {
      return type.types.map((part) =>
        portableTypeText(checker, part, location, projectRoot, seen, depth + 1)
      ).join(" & ");
    }
    if (checker.isArrayType && checker.isArrayType(type)) {
      const element = checker.getTypeArguments(type)[0];
      const text = portableTypeText(checker, element, location, projectRoot, seen, depth + 1);
      return `Array<${text}>`;
    }
    const symbol = type.aliasSymbol || type.symbol;
    if (projectLocalSymbol(symbol, projectRoot)) {
      seen.add(type);
      const members = checker.getPropertiesOfType(type).slice(0, 64).map((property) => {
        const propertyType = checker.getTypeOfSymbolAtLocation(property, location);
        const propertyText = portableTypeText(
          checker,
          propertyType,
          location,
          projectRoot,
          seen,
          depth + 1,
        );
        const optional = property.flags & ts.SymbolFlags.Optional ? "?" : "";
        return `${JSON.stringify(property.getName())}${optional}: ${propertyText};`;
      });
      seen.delete(type);
      return members.length === 0 ? "unknown" : `{ ${members.join(" ")} }`;
    }
    const typeArguments = type.aliasTypeArguments || type.typeArguments;
    if (
      typeArguments && typeArguments.length > 0 &&
      typeArguments.some((argument) => projectLocalSymbol(argument.aliasSymbol || argument.symbol, projectRoot))
    ) {
      const name = symbol && symbol.getName();
      if (name && /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name)) {
        const argumentsText = typeArguments.map((argument) =>
          portableTypeText(checker, argument, location, projectRoot, seen, depth + 1)
        ).join(", ");
        return `${name}<${argumentsText}>`;
      }
      return "unknown";
    }
    return checker.typeToString(
      type,
      location,
      ts.TypeFormatFlags.NoTruncation |
        ts.TypeFormatFlags.UseStructuralFallback |
        ts.TypeFormatFlags.WriteClassExpressionAsTypeLiteral |
        ts.TypeFormatFlags.UseTypeOfFunction,
    );
  }

  function inferredVarsDeclaration(program, projectRoot) {
    const checker = program.getTypeChecker();
    const properties = new Map();
    for (const sourceFile of inlineSourceFiles(program, projectRoot)) {
      const visit = (node) => {
        const write = varsPropertyWrite(node);
        if (write && isInlineVarsSymbol(checker, write.receiver, projectRoot)) {
          let type = checker.getTypeAtLocation(write.value);
          if (typeof checker.getWidenedType === "function") {
            type = checker.getWidenedType(type);
          }
          const text = portableTypeText(
            checker,
            type,
            write.value,
            projectRoot,
            new Set(),
            0,
          );
          if (text && text.length <= MAX_INFERRED_VAR_TYPE_LENGTH) {
            let types = properties.get(write.name);
            if (!types) {
              if (properties.size === MAX_INFERRED_VAR_PROPERTIES) return;
              types = new Set();
              properties.set(write.name, types);
            }
            types.add(text);
          }
        }
        ts.forEachChild(node, visit);
      };
      visit(sourceFile);
    }
    if (properties.size === 0) return undefined;
    const members = Array.from(properties.entries())
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([name, types]) =>
        `    ${JSON.stringify(name)}: ${Array.from(types).sort().join(" | ")};`
      )
      .join("\n");
    return `export {};\ndeclare global {\n  interface SmudgyUserVars {\n${members}\n  }\n}\n`;
  }

  function hasExportModifier(node) {
    return !!node.modifiers && node.modifiers.some((modifier) =>
      modifier.kind === ts.SyntaxKind.ExportKeyword ||
      modifier.kind === ts.SyntaxKind.DefaultKeyword
    );
  }

  function inlineRuntimeDiagnostics(fileName) {
    const projectRoot = inlineProjectRoot(documents);
    const sourceFile = service.getProgram() && service.getProgram().getSourceFile(fileName);
    if (
      !projectRoot || !sourceFile ||
      !sourceFile.fileName.startsWith(projectRoot) ||
      !documents[normalize(sourceFile.fileName)] ||
      sourceFile.isDeclarationFile
    ) {
      return [];
    }
    const values = [];
    const add = (node, code, messageText) => values.push({
      file: sourceFile,
      start: node.getStart(sourceFile),
      length: Math.max(1, node.getWidth(sourceFile)),
      category: ts.DiagnosticCategory.Error,
      code,
      messageText,
    });
    for (const statement of sourceFile.statements) {
      if (
        ts.isImportDeclaration(statement) ||
        ts.isImportEqualsDeclaration(statement) ||
        ts.isExportDeclaration(statement) ||
        ts.isExportAssignment(statement) ||
        hasExportModifier(statement)
      ) {
        add(
          statement,
          INLINE_MODULE_SYNTAX_CODE,
          "Static imports and exports are unavailable in inline automations; move this code to a module or package.",
        );
      }
    }
    const visitImportMeta = (node) => {
      if (
        ts.isMetaProperty(node) &&
        node.keywordToken === ts.SyntaxKind.ImportKeyword
      ) {
        add(
          node,
          INLINE_MODULE_SYNTAX_CODE,
          "import.meta is unavailable in inline automations; move this code to a module or package.",
        );
        return;
      }
      ts.forEachChild(node, visitImportMeta);
    };
    visitImportMeta(sourceFile);
    const visitAwait = (node) => {
      if (node !== sourceFile && ts.isFunctionLike(node)) return;
      if (
        ts.isAwaitExpression(node) ||
        (ts.isForOfStatement(node) && node.awaitModifier) ||
        (ts.isVariableDeclarationList(node) && ts.isVarAwaitUsing(node))
      ) {
        add(
          node,
          INLINE_TOP_LEVEL_AWAIT_CODE,
          "Top-level await is unavailable in inline automations; move this code to an async function, module, or package.",
        );
        return;
      }
      ts.forEachChild(node, visitAwait);
    };
    visitAwait(sourceFile);
    return values;
  }

  function isRuntimePackageSpecifier(value) {
    return (value.startsWith("npm:") && value.length > "npm:".length) ||
      (value.startsWith("jsr:") && value.length > "jsr:".length) ||
      (value.startsWith("@") && value.length > 1) ||
      (value.startsWith("smudgy:@") && value.length > "smudgy:@".length) ||
      (value.startsWith("smudgy://") && value.length > "smudgy://".length);
  }

  function stringLiteralAtPosition(sourceFile, position) {
    let found;
    const visit = (node) => {
      if (found || position < node.getFullStart() || position >= node.end) return;
      if (ts.isStringLiteralLike(node)) {
        found = node;
        return;
      }
      ts.forEachChild(node, visit);
    };
    visit(sourceFile);
    return found;
  }

  function isRuntimePackageMissingModuleDiagnostic(value, sourceFile) {
    if (value.code !== 2307 || !sourceFile || typeof value.start !== "number") return false;
    const literal = stringLiteralAtPosition(sourceFile, value.start);
    return !!literal && isRuntimePackageSpecifier(literal.text);
  }

  function authoredDiagnostics(fileName) {
    const program = service.getProgram();
    const sourceFile = program && program.getSourceFile(fileName);
    // npm:, jsr:, and Smudgy package imports (including @name) resolve at runtime. This
    // synchronous, snapshot-only TypeScript host cannot ask those resolvers to fetch a
    // package, so TS2307 would be a false claim that valid runtime syntax is missing.
    // Exact ambient/package declarations still participate normally and provide their
    // completion, hover, and definition information; every other diagnostic is retained.
    return service.getSyntacticDiagnostics(fileName)
      .concat(service.getSemanticDiagnostics(fileName))
      .concat(service.getSuggestionDiagnostics(fileName))
      .filter((value) => !isRuntimePackageMissingModuleDiagnostic(value, sourceFile));
  }

  function lineTerminatorLength(text, index) {
    const code = text.charCodeAt(index);
    if (code === 13) {
      return text.charCodeAt(index + 1) === 10 ? 2 : 1;
    }
    return code === 10 || code === 0x2028 || code === 0x2029 ? 1 : 0;
  }

  function offsetAt(fileName, position) {
    const found = entry(fileName);
    if (!found) throw new Error("unknown file: " + fileName);
    const requestedLine = Number(position.line);
    if (!Number.isInteger(requestedLine) || requestedLine < 0) {
      throw new Error("invalid UTF-16 line");
    }
    let start = 0;
    for (let line = 0; line < requestedLine; line += 1) {
      let terminator = start;
      while (terminator < found.text.length && lineTerminatorLength(found.text, terminator) === 0) {
        terminator += 1;
      }
      if (terminator >= found.text.length) throw new Error("line outside file");
      start = terminator + lineTerminatorLength(found.text, terminator);
    }
    let end = start;
    while (end < found.text.length && lineTerminatorLength(found.text, end) === 0) {
      end += 1;
    }
    const character = Number(position.character);
    if (!Number.isInteger(character) || character < 0 || character > end - start) {
      throw new Error("UTF-16 character outside line");
    }
    const offset = start + character;
    if (offset > start && offset < end) {
      const previous = found.text.charCodeAt(offset - 1);
      const current = found.text.charCodeAt(offset);
      if (previous >= 0xd800 && previous <= 0xdbff && current >= 0xdc00 && current <= 0xdfff) {
        throw new Error("UTF-16 character splits a surrogate pair");
      }
    }
    return offset;
  }

  function positionAt(fileName, offset) {
    const found = entry(fileName);
    if (!found) return undefined;
    const bounded = Math.max(0, Math.min(offset, found.text.length));
    let line = 0;
    let lineStart = 0;
    for (let index = 0; index < bounded;) {
      const terminatorLength = lineTerminatorLength(found.text, index);
      if (terminatorLength === 0) {
        index += 1;
        continue;
      }
      if (bounded < index + terminatorLength) {
        throw new Error("offset splits a CRLF line terminator");
      }
      line += 1;
      index += terminatorLength;
      lineStart = index;
    }
    return { line, character: bounded - lineStart };
  }

  function range(fileName, textSpan) {
    if (!textSpan || !entry(fileName)) return undefined;
    return {
      start: positionAt(fileName, textSpan.start),
      end: positionAt(fileName, textSpan.start + textSpan.length),
    };
  }

  function display(parts) {
    return ts.displayPartsToString(parts || []);
  }

  function escapeMarkdownLabel(value) {
    const punctuation = "\\`*_{}[]()<>#+-.!|";
    let escaped = "";
    for (const character of String(value)) {
      if (punctuation.includes(character)) escaped += "\\";
      escaped += character;
    }
    return escaped;
  }

  function markdownCodeSpan(value) {
    const text = String(value).replace(/\s+/g, " ").trim();
    const runs = text.match(/`+/g) || [];
    const fence = "`".repeat(
      runs.reduce((longest, run) => Math.max(longest, run.length), 0) + 1,
    );
    const needsPadding = text.startsWith("`") || text.endsWith("`") ||
      (text.startsWith(" ") && text.endsWith(" "));
    return fence + (needsPadding ? ` ${text} ` : text) + fence;
  }

  const MAX_JSDOC_LINK_TARGET_SCAN = 4096;

  function topLevelLinkDelimiter(value) {
    let quote = "";
    let escaped = false;
    let depth = 0;
    const limit = Math.min(value.length, MAX_JSDOC_LINK_TARGET_SCAN);
    for (let index = 0; index < limit; index += 1) {
      const character = value[index];
      if (quote) {
        if (escaped) escaped = false;
        else if (character === "\\") escaped = true;
        else if (character === quote) quote = "";
        continue;
      }
      if (character === "\"" || character === "'" || character === "`") {
        quote = character;
      } else if (
        character === "(" || character === "[" || character === "{" || character === "<"
      ) {
        depth += 1;
      } else if (
        character === ")" || character === "]" || character === "}" || character === ">"
      ) {
        depth = Math.max(0, depth - 1);
      } else if (character === "|" && depth === 0) {
        return index;
      }
    }
    return -1;
  }

  function skipLinkWhitespace(value, start, limit) {
    let index = start;
    while (index < limit && /\s/.test(value[index])) index += 1;
    return index;
  }

  function quotedLinkTargetEnd(value, start, limit) {
    const quote = value[start];
    if (quote !== "\"" && quote !== "'" && quote !== "`") return -1;
    let escaped = false;
    for (let index = start + 1; index < limit; index += 1) {
      const character = value[index];
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === quote) return index + 1;
    }
    return -1;
  }

  function balancedLinkTargetEnd(value, start, limit) {
    let quote = "";
    let escaped = false;
    let depth = 0;
    for (let index = start; index < limit; index += 1) {
      const character = value[index];
      if (quote) {
        if (escaped) escaped = false;
        else if (character === "\\") escaped = true;
        else if (character === quote) quote = "";
        continue;
      }
      if (character === "\"" || character === "'" || character === "`") {
        quote = character;
      } else if (
        character === "(" || character === "[" || character === "{" || character === "<"
      ) {
        depth += 1;
      } else if (
        character === ")" || character === "]" || character === "}" || character === ">"
      ) {
        depth = Math.max(0, depth - 1);
      } else if (depth === 0 && (/\s/.test(character) || character === "|")) {
        return index;
      }
    }
    return limit;
  }

  function normalizedColonNamepathEnd(value, limit) {
    let prefixEnd = 0;
    while (prefixEnd < limit && /[\w$.-]/.test(value[prefixEnd])) prefixEnd += 1;
    if (prefixEnd === 0) return -1;
    const colon = skipLinkWhitespace(value, prefixEnd, limit);
    if (value[colon] !== ":") return -1;
    const target = skipLinkWhitespace(value, colon + 1, limit);
    return balancedLinkTargetEnd(value, target, limit);
  }

  // TypeScript normalizes unresolved JSDoc namepaths before emitting linkText:
  // import("pkg").Type becomes `import ("pkg").Type`, and standard colon
  // namepaths such as module:"pkg" or event:ready gain a space before `:`.
  // Generic references also retain spaces inside balanced `<...>`. Recover
  // those bounded target shapes so their whitespace and union pipes are not
  // mistaken for custom-label separators.
  function unboundLinkTargetEnd(value) {
    const limit = Math.min(value.length, MAX_JSDOC_LINK_TARGET_SCAN);
    if (value.startsWith("import")) {
      let index = skipLinkWhitespace(value, "import".length, limit);
      if (value[index] === "(") {
        index = skipLinkWhitespace(value, index + 1, limit);
        const quotedEnd = quotedLinkTargetEnd(value, index, limit);
        if (quotedEnd >= 0) {
          index = skipLinkWhitespace(value, quotedEnd, limit);
          if (value[index] === ")") {
            return balancedLinkTargetEnd(value, index + 1, limit);
          }
        }
      }
    }

    const colonNamepathEnd = normalizedColonNamepathEnd(value, limit);
    return colonNamepathEnd >= 0
      ? colonNamepathEnd
      : balancedLinkTargetEnd(value, 0, limit);
  }

  function unboundLinkLabel(value) {
    const text = String(value).trim();
    const delimiter = topLevelLinkDelimiter(text);
    if (delimiter >= 0) {
      const target = text.slice(0, delimiter).trim();
      return text.slice(delimiter + 1).trim() || target;
    }
    const targetEnd = unboundLinkTargetEnd(text);
    const alternate = text.slice(targetEnd).trim();
    return alternate || text.slice(0, targetEnd).trim();
  }

  const NORMALIZED_COLON_LINK_PREFIXES = Object.freeze([
    "event",
    "external",
    "module",
    "namespace",
  ]);

  function resolvedLinkLabel(name, rawAlternate) {
    const alternate = rawAlternate.trim();
    if (!alternate) return unboundLinkLabel(name);
    // A resolvable colon-namepath prefix can become linkName while TypeScript
    // leaves `:target label` in an adjacent linkText. Preserve leading-space
    // evidence when TypeScript provides it, and constrain the otherwise
    // ambiguous colon form to standard JSDoc namepath prefixes. Every other
    // non-empty linkText is the authoritative custom label, including labels
    // beginning with punctuation.
    const adjacent = rawAlternate.length === alternate.length;
    const standardPrefix = NORMALIZED_COLON_LINK_PREFIXES.includes(
      name.trim().toLowerCase(),
    );
    return adjacent && alternate[0] === ":" && standardPrefix
      ? unboundLinkLabel(name + alternate)
      : alternate;
  }

  function markdownDisplay(parts, linkState) {
    const values = parts || [];
    let markdown = "";
    for (let index = 0; index < values.length; index += 1) {
      const part = values[index];
      const match = part.kind === "link" &&
        /^\{@(link|linkcode|linkplain)\s+/.exec(part.text);
      if (!match) {
        markdown += part.text;
        continue;
      }

      let end = index + 1;
      while (
        end < values.length &&
        !(values[end].kind === "link" && values[end].text.trim() === "}")
      ) {
        end += 1;
      }
      if (end >= values.length) {
        markdown += part.text;
        continue;
      }

      const body = values.slice(index + 1, end);
      const name = body.filter((value) => value.kind === "linkName")
        .map((value) => value.text).join("").trim();
      const rawAlternate = body.filter((value) => value.kind === "linkText")
        .map((value) => value.text).join("");
      const alternate = rawAlternate.trim();
      // TypeScript separates a resolved target into linkName + linkText, but
      // collapses unresolved symbols and external URLs into one linkText after
      // removing either the whitespace or `|` label delimiter. In that shape,
      // recover the normalized target shape before selecting its custom label.
      const label = name
        ? resolvedLinkLabel(name, rawAlternate)
        : unboundLinkLabel(alternate || display(body));
      if (!label) {
        markdown += display(values.slice(index, end + 1));
        index = end;
        continue;
      }

      linkState.next += 1;
      const rendered = match[1] === "linkcode"
        ? markdownCodeSpan(label)
        : escapeMarkdownLabel(label);
      markdown += `[${rendered}](#smudgy-jsdoc-link-${linkState.next})`;
      index = end;
    }
    return markdown;
  }

  function markdownDocumentation(documentation, tags, linkState) {
    const body = markdownDisplay(documentation, linkState);
    const tagList = (tags || []).map((tag) => {
      const text = markdownDisplay(tag.text, linkState);
      const label = markdownCodeSpan(`@${tag.name}`);
      if (!text) return `- ${label}`;
      // A fenced @example (and any other multiline block) must begin after a
      // blank line. Prefixing it on a list-item line makes the backticks
      // literal CommonMark text, so iced never receives a highlighted block.
      if (text.includes("\n") || /^(?:```|~~~)/.test(text.trimStart())) {
        return `${label}\n\n${text}`;
      }
      return `- ${label} ${text}`;
    }).join("\n\n");
    return [body, tagList].filter(Boolean).join("\n\n");
  }

  function diagnostic(value) {
    const fileName = value.file && normalize(value.file.fileName);
    return {
      fileName,
      code: value.code,
      category: ts.DiagnosticCategory[value.category],
      message: ts.flattenDiagnosticMessageText(value.messageText, "\n"),
      range: fileName && value.start !== undefined && value.length !== undefined
        ? range(fileName, { start: value.start, length: value.length })
        : undefined,
    };
  }

  function handle(request) {
    const method = request.method;
    const params = request.params || {};
    if (disposed) throw new Error("LanguageService is disposed");
    if (method === "initialize") {
      if (initialized) throw new Error("LanguageService is already initialized");
      libraries = makeTable(params.libraries || {}, undefined);
      // TypeScript's triple-slash `lib` parser consults an internal name map before
      // asking the compiler host for a file. Register only Smudgy's embedded Deno libs;
      // absent host libs such as DOM remain unresolvable and cannot widen the project.
      if (ts.libMap && typeof ts.libMap.set === "function") {
        for (const fileName of Object.keys(libraries)) {
          const match = /^\/lib\.(deno(?:[._][^.]+)*)\.d\.ts$/i.exec(fileName);
          if (match) ts.libMap.set(match[1].toLocaleLowerCase(), fileName.slice(1));
        }
      }
      libraryRoots = Object.freeze(Array.from(new Set(
        (params.libraryRoots || []).map(normalize),
      )).sort());
      for (const root of libraryRoots) {
        if (!libraries[root]) throw new Error("unknown library root: " + root);
      }
      roots = libraryRoots;
      initialized = true;
      projectVersion += 1;
      return { typescriptVersion: ts.version };
    }
    if (!initialized) throw new Error("LanguageService is not initialized");
    if (method === "dispose") {
      service.dispose();
      libraries = Object.freeze({});
      libraryRoots = Object.freeze([]);
      documents = Object.freeze({});
      roots = Object.freeze([]);
      initialized = false;
      disposed = true;
      return { disposed: true };
    }
    if (poisoned) throw new Error("LanguageService snapshot is poisoned");
    if (method === "replaceProject") {
      const previousDocuments = documents;
      const previousRoots = roots;
      const previousProjectVersion = projectVersion;
      try {
        documents = makeTable(params.files || {}, documents);
        roots = Object.freeze(Array.from(new Set(
          libraryRoots.concat(Object.keys(documents)),
        )).sort());
        projectVersion += 1;
        // Force TypeScript to observe the new immutable snapshot before acknowledging it.
        const program = service.getProgram();
        const projectRoot = inlineProjectRoot(documents);
        const varsDeclaration = projectRoot && program &&
          inferredVarsDeclaration(program, projectRoot);
        if (varsDeclaration) {
          const generatedName = projectRoot + "inline/vars.generated.d.ts";
          documents = makeTable({
            ...(params.files || {}),
            [generatedName]: varsDeclaration,
          }, documents);
          roots = Object.freeze(Array.from(new Set(
            libraryRoots.concat(Object.keys(documents)),
          )).sort());
          projectVersion += 1;
          service.getProgram();
        }
      } catch (error) {
        documents = previousDocuments;
        roots = previousRoots;
        projectVersion = previousProjectVersion;
        try {
          service.getProgram();
        } catch (_) {
          poisoned = true;
          throw new Error("LanguageService snapshot rollback failed");
        }
        throw error;
      }
      return { projectVersion, roots: roots.slice() };
    }
    if (method === "diagnostics") {
      const fileName = normalize(params.fileName);
      const values = authoredDiagnostics(fileName)
        .concat(inlineRuntimeDiagnostics(fileName));
      return { diagnostics: values.map(diagnostic) };
    }
    if (method === "completion") {
      const fileName = normalize(params.fileName);
      const position = offsetAt(fileName, params.position);
      const info = service.getCompletionsAtPosition(fileName, position, {
        includeCompletionsForModuleExports: false,
        includeCompletionsWithInsertText: true,
      });
      // This protocol cannot yet apply completion code actions. Never surface an
      // auto-import entry whose insertion would therefore leave invalid source.
      // Explicit import-clause and member completions carry neither marker.
      const sourceEntries = (info && info.entries || []).filter(
        (value) => !value.source && !value.hasAction,
      );
      // TypeScript returns a broad candidate set and expects the client to
      // filter it against the replacement-span prefix. The Automations UI is
      // intentionally small, so filter before its bounded visible list instead
      // of allowing alphabetically early keywords to hide the requested name.
      const sourceText = entry(fileName).text;
      const sharedSpan = info && info.optionalReplacementSpan;
      const matchingEntries = sourceEntries.filter((value) => {
        const span = value.replacementSpan || sharedSpan;
        if (!span || span.start > position) return true;
        const prefix = sourceText.slice(span.start, position).toLocaleLowerCase();
        if (!prefix) return true;
        const candidate = String(value.filterText || value.insertText || value.name)
          .toLocaleLowerCase();
        return candidate.startsWith(prefix);
      });
      const entries = matchingEntries.slice(0, 500).map((value) => ({
        name: value.name,
        kind: value.kind,
        sortText: value.sortText,
        insertText: value.insertText,
        replacementSpan: range(
          fileName,
          value.replacementSpan || (info && info.optionalReplacementSpan),
        ),
        deprecated: !!(value.kindModifiers && value.kindModifiers.includes("deprecated")),
      }));
      return {
        isIncomplete: !!(info && info.isIncomplete) || matchingEntries.length > entries.length,
        entries,
      };
    }
    if (method === "hover") {
      const fileName = normalize(params.fileName);
      const position = offsetAt(fileName, params.position);
      const info = service.getQuickInfoAtPosition(fileName, position);
      if (!info) return { hover: null };
      const documentation = markdownDocumentation(
        info.documentation,
        info.tags,
        { next: 0 },
      );
      return {
        hover: {
          range: range(fileName, info.textSpan),
          display: display(info.displayParts),
          documentation,
        },
      };
    }
    if (method === "signatureHelp") {
      const fileName = normalize(params.fileName);
      const position = offsetAt(fileName, params.position);
      const info = service.getSignatureHelpItems(fileName, position);
      if (!info || !info.items || info.items.length === 0) {
        return { signatureHelp: null };
      }
      const selectedIndex = Number(info.selectedItemIndex);
      if (
        !Number.isInteger(selectedIndex) || selectedIndex < 0 ||
        selectedIndex >= info.items.length
      ) {
        throw new Error("invalid selected signature index");
      }
      if (info.items.length > MAX_SIGNATURE_HELP_COUNT) {
        return { signatureHelp: null };
      }
      const selected = info.items[selectedIndex];
      const sourceParameters = selected.parameters || [];
      if (sourceParameters.length > MAX_SIGNATURE_HELP_PARAMETERS) {
        return { signatureHelp: null };
      }
      const retainedParameters = sourceParameters;
      const isRestParameter = (parameter, index) => !!parameter.isRest ||
        (!!selected.isVariadic && index === sourceParameters.length - 1);
      const argumentIndex = Number(info.argumentIndex);
      let activeParameter = null;
      if (
        Number.isInteger(argumentIndex) && argumentIndex >= 0 &&
        retainedParameters.length > 0
      ) {
        if (argumentIndex < retainedParameters.length) {
          activeParameter = argumentIndex;
        } else if (selected.isVariadic) {
          const restIndex = retainedParameters.findIndex(isRestParameter);
          if (restIndex >= 0 && argumentIndex >= restIndex) activeParameter = restIndex;
        }
      }
      const linkState = { next: 0 };
      const documentation = markdownDocumentation(
        selected.documentation,
        (selected.tags || []).filter((tag) => tag.name !== "param"),
        linkState,
      );
      const argumentCount = Number(info.argumentCount);
      if (
        !Number.isInteger(argumentCount) || argumentCount < 0 ||
        argumentCount > MAX_SIGNATURE_HELP_COUNT
      ) {
        return { signatureHelp: null };
      }
      return {
        signatureHelp: {
          applicableRange: range(fileName, info.applicableSpan),
          prefix: display(selected.prefixDisplayParts),
          separator: display(selected.separatorDisplayParts),
          suffix: display(selected.suffixDisplayParts),
          parameters: retainedParameters.map((parameter, index) => ({
            label: display(parameter.displayParts),
            documentation: markdownDisplay(parameter.documentation, linkState),
            isOptional: !!parameter.isOptional,
            isRest: isRestParameter(parameter, index),
          })),
          activeParameter,
          selectedSignature: selectedIndex,
          signatureCount: info.items.length,
          argumentCount,
          documentation,
        },
      };
    }
    if (method === "definition") {
      const fileName = normalize(params.fileName);
      const position = offsetAt(fileName, params.position);
      const result = service.getDefinitionAndBoundSpan(fileName, position);
      return {
        definitions: (result && result.definitions || []).map((value) => ({
          fileName: normalize(value.fileName),
          range: range(value.fileName, value.textSpan),
        })),
      };
    }
    if (method === "format") {
      const fileName = normalize(params.fileName);
      const options = params.options || {};
      const edits = service.getFormattingEditsForDocument(fileName, {
        indentSize: Number(options.tabSize) || 2,
        tabSize: Number(options.tabSize) || 2,
        convertTabsToSpaces: options.insertSpaces !== false,
        semicolons: ts.SemicolonPreference.Insert,
        newLineCharacter: "\n",
      });
      return {
        edits: edits.map((edit) => ({
          range: range(fileName, edit.span),
          newText: edit.newText,
        })),
      };
    }
    throw new Error("unknown LanguageService method: " + method);
  }

  globalThis.__SMUDGY_LANGUAGE_SERVICE_HANDLE = function (requestJson) {
    try {
      return JSON.stringify({ ok: true, result: handle(JSON.parse(requestJson)) });
    } catch (error) {
      return JSON.stringify({
        ok: false,
        error: error && error.stack ? String(error.stack) : String(error),
      });
    }
  };
})();
