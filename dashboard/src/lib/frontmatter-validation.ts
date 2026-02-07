export function validateFrontmatterBlock(content: string): string | null {
  const lines = content.split(/\r?\n/);
  if (lines[0]?.trim() !== '---') {
    return null;
  }

  let endLineIndex = -1;
  for (let i = 1; i < lines.length; i += 1) {
    const rawLine = lines[i];
    if (rawLine.trim() === '---' && rawLine.trimStart() === rawLine) {
      endLineIndex = i;
      break;
    }
  }

  if (endLineIndex === -1) {
    return 'Frontmatter is missing a closing "---"';
  }

  const yamlLines = lines.slice(1, endLineIndex);
  let expectingListItem = false;
  let inMultilineBlock = false;
  let multilineIndent = 0;

  for (let i = 0; i < yamlLines.length; i += 1) {
    const rawLine = yamlLines[i];
    const line = rawLine.trim();
    const leadingWhitespace = rawLine.length - rawLine.trimStart().length;

    if (inMultilineBlock) {
      if (!line) {
        continue;
      }
      if (leadingWhitespace > multilineIndent) {
        continue;
      }
      inMultilineBlock = false;
    }

    if (!line || line.startsWith('#')) {
      continue;
    }

    if (line.startsWith('-')) {
      if (!expectingListItem) {
        return `Invalid frontmatter at line ${i + 2}: list item without a key`;
      }
      continue;
    }

    expectingListItem = false;
    const match = line.match(/^([A-Za-z0-9_-]+):(.*)$/);
    if (!match) {
      return `Invalid frontmatter at line ${i + 2}: "${rawLine}"`;
    }

    const value = match[2].trim();
    if (value === '') {
      expectingListItem = true;
    } else if (value.startsWith('|') || value.startsWith('>')) {
      inMultilineBlock = true;
      multilineIndent = leadingWhitespace;
    }
  }

  return null;
}

