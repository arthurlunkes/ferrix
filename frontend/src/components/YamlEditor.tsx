import React, { useRef } from "react";

interface YamlEditorProps {
  value: string;
  onChange: (value: string) => void;
}

type TokenKind = "key" | "string" | "number" | "boolean" | "comment";

const tokenClass: Record<TokenKind, string> = {
  key: "text-sky-400",
  string: "text-emerald-400",
  number: "text-amber-400",
  boolean: "text-violet-400",
  comment: "text-content-muted italic",
};

function splitComment(line: string): [string, string] {
  let quote: "'" | '"' | null = null;
  let escaped = false;

  for (let index = 0; index < line.length; index += 1) {
    const character = line[index];

    if (quote === '"') {
      if (escaped) escaped = false;
      else if (character === "\\") escaped = true;
      else if (character === '"') quote = null;
      continue;
    }

    if (quote === "'") {
      if (character === "'" && line[index + 1] === "'") index += 1;
      else if (character === "'") quote = null;
      continue;
    }

    if (character === '"' || character === "'") quote = character;
    else if (character === "#" && (index === 0 || /\s/.test(line[index - 1]))) {
      return [line.slice(0, index), line.slice(index)];
    }
  }

  return [line, ""];
}

function renderValue(value: string, lineKey: string): React.ReactNode[] {
  const tokenPattern =
    /("(?:\\.|[^"\\])*"|'(?:''|[^'])*'|\b(?:true|false|yes|no|on|off|null|~)\b|-?\b(?:0x[\da-f]+|0o[0-7]+|\d+(?:\.\d+)?(?:e[+-]?\d+)?)\b)/gi;
  const nodes: React.ReactNode[] = [];
  let cursor = 0;
  let match: RegExpExecArray | null;

  const appendString = (text: string, key: string) => {
    if (!text) return;
    nodes.push(
      text.trim()
        ? <span className={tokenClass.string} key={key}>{text}</span>
        : text,
    );
  };

  while ((match = tokenPattern.exec(value)) !== null) {
    appendString(value.slice(cursor, match.index), `${lineKey}-text-${cursor}`);

    const token = match[0];
    let kind: TokenKind = "string";
    if (/^(true|false|yes|no|on|off|null|~)$/i.test(token)) {
      kind = "boolean";
    } else if (/^-?\b(?:0x[\da-f]+|0o[0-7]+|\d+(?:\.\d+)?(?:e[+-]?\d+)?)\b$/i.test(token)) {
      kind = "number";
    }

    nodes.push(
      <span className={tokenClass[kind]} key={`${lineKey}-${match.index}`}>
        {token}
      </span>,
    );
    cursor = tokenPattern.lastIndex;
  }

  appendString(value.slice(cursor), `${lineKey}-text-${cursor}`);
  return nodes;
}

function highlightLine(line: string, lineNumber: number): React.ReactNode {
  const [content, comment] = splitComment(line);
  const keyMatch = /^(\s*(?:-\s+)?)([^:]+?)(:\s*)(.*)$/.exec(content);

  return (
    <>
      {keyMatch ? (
        <>
          {keyMatch[1]}
          <span className={tokenClass.key}>{keyMatch[2]}</span>
          {keyMatch[3]}
          {renderValue(keyMatch[4], `line-${lineNumber}`)}
        </>
      ) : (
        renderValue(content, `line-${lineNumber}`)
      )}
      {comment && <span className={tokenClass.comment}>{comment}</span>}
    </>
  );
}

export const YamlEditor: React.FC<YamlEditorProps> = ({ value, onChange }) => {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const highlightRef = useRef<HTMLPreElement>(null);
  const lineNumbersRef = useRef<HTMLDivElement>(null);
  const lines = value.split("\n");

  const syncScroll = () => {
    const textarea = textareaRef.current;
    if (!textarea) return;

    if (highlightRef.current) {
      highlightRef.current.scrollTop = textarea.scrollTop;
      highlightRef.current.scrollLeft = textarea.scrollLeft;
    }
    if (lineNumbersRef.current) {
      lineNumbersRef.current.scrollTop = textarea.scrollTop;
    }
  };

  return (
    <div className="relative h-[672px] min-h-[240px] resize-y overflow-hidden rounded-xl bg-surface-0">
      <div
        ref={lineNumbersRef}
        aria-hidden="true"
        className="absolute inset-y-0 left-0 z-20 w-12 overflow-hidden border-r border-border bg-surface-1 px-2 pt-3 text-right font-mono text-xs leading-6 text-content-muted select-none"
      >
        {lines.map((_, index) => (
          <div key={index}>{index + 1}</div>
        ))}
      </div>

      <pre
        ref={highlightRef}
        aria-hidden="true"
        className="pointer-events-none absolute inset-y-0 left-12 right-0 z-0 m-0 overflow-hidden px-3 py-3 font-mono text-xs leading-6"
        style={{ tabSize: 2, whiteSpace: "pre" }}
      >
        {lines.map((line, index) => (
          <div key={index}>{highlightLine(line, index)}</div>
        ))}
      </pre>

      <textarea
        ref={textareaRef}
        aria-label="YAML configuration editor"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        onScroll={syncScroll}
        spellCheck={false}
        autoCapitalize="off"
        autoComplete="off"
        autoCorrect="off"
        wrap="off"
        className="absolute inset-y-0 left-12 right-0 z-10 h-full w-[calc(100%-3rem)] resize-none overflow-auto border-0 bg-transparent px-3 py-3 font-mono text-xs leading-6 text-transparent caret-content-primary focus:outline-none"
        style={{
          tabSize: 2,
          whiteSpace: "pre",
          WebkitTextFillColor: "transparent",
          caretColor: "var(--text-primary)",
        }}
      />
    </div>
  );
};
