// The frame's WebView is Chromium 74 and cannot be updated. A newer browser runs the screen
// happily, so nothing else would notice when the screen starts using something 74 lacks: it
// would only break on the frame. This reads the screen's own files for anything newer.
// (The management page runs on phones and computers, which are up to date; it is not held back.)

import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { test } from 'node:test';

const SCREEN = new URL('../screen/', import.meta.url);
const files = (kind) => readdirSync(SCREEN).filter((name) => name.endsWith(kind))
  .map((name) => ({ name, text: readFileSync(new URL(name, SCREEN), 'utf8') }));

/// The code of a script with its comments, strings, template text and regular expressions
/// blanked out (lengths kept, so places still line up), leaving only what the engine parses.
export function codeOnly(source) {
  let out = '';
  let i = 0;
  // Whether a `/` here would start a regular expression rather than divide.
  let expectValue = true;
  // Template literals being read, and how deep in braces each `${` is.
  const templates = [];
  const blank = (text) => text.replace(/[^\n]/g, ' ');
  while (i < source.length) {
    const c = source[i], next = source[i + 1];
    if (c === '/' && next === '/') {
      const end = source.indexOf('\n', i);
      const stop = end < 0 ? source.length : end;
      out += blank(source.slice(i, stop));
      i = stop;
    } else if (c === '/' && next === '*') {
      const stop = source.indexOf('*/', i + 2) + 2;
      out += blank(source.slice(i, stop));
      i = stop;
    } else if (c === '"' || c === "'") {
      let j = i + 1;
      while (source[j] !== c) j += source[j] === '\\' ? 2 : 1;
      out += c + blank(source.slice(i + 1, j)) + c;
      i = j + 1;
      expectValue = false;
    } else if (c === '`' || (c === '}' && templates.length && templates[templates.length - 1] === 0)) {
      // Template text runs to the closing backtick or the next `${`.
      if (c === '}') templates.pop();
      let j = i + 1;
      while (source[j] !== '`' && !(source[j] === '$' && source[j + 1] === '{')) j += source[j] === '\\' ? 2 : 1;
      out += c + blank(source.slice(i + 1, j));
      if (source[j] === '`') {
        out += '`';
        i = j + 1;
        expectValue = false;
      } else {
        out += '${';
        templates.push(0);
        i = j + 2;
        expectValue = true;
      }
    } else if (c === '/' && expectValue) {
      let j = i + 1, inClass = false;
      while (source[j] !== '/' || inClass) {
        if (source[j] === '\\') j++;
        else if (source[j] === '[') inClass = true;
        else if (source[j] === ']') inClass = false;
        j++;
      }
      const close = j;
      j++;
      while (/[a-z]/.test(source[j] || '')) j++;
      // The pattern and its flags are not code.
      out += '/' + blank(source.slice(i + 1, close)) + '/' + blank(source.slice(close + 1, j));
      i = j;
      expectValue = false;
    } else {
      if (templates.length) {
        if (c === '{') templates[templates.length - 1]++;
        else if (c === '}') templates[templates.length - 1]--;
      }
      if (/[\s]/.test(c)) {
        // Space says nothing about what comes next.
      } else if (/[\w$]/.test(c)) {
        let j = i;
        while (/[\w$]/.test(source[j] || '')) j++;
        const word = source.slice(i, j);
        out += word;
        i = j;
        expectValue = /^(return|typeof|instanceof|in|of|new|delete|void|throw|case|do|else|yield|await)$/.test(word);
        continue;
      } else {
        expectValue = !/[)\]}]/.test(c);
      }
      out += c;
      i++;
    }
  }
  return out;
}

/// The line a place in a text is on, counting from 1.
const lineOf = (text, at) => text.slice(0, at).split('\n').length;

/// Where `rules` find something in `text`, as "file:line what".
function findings(name, text, rules) {
  const found = [];
  for (const [pattern, what] of rules) {
    for (const match of text.matchAll(pattern)) found.push(`${name}:${lineOf(text, match.index)} ${what}: ${match[0].trim()}`);
  }
  return found;
}

// Each with the Chrome version that first had it.
const SCRIPT_RULES = [
  [/\?\.(?!\d)/g, 'optional chaining (Chrome 80)'],
  [/\?\?/g, 'nullish coalescing (Chrome 80)'],
  [/(\|\||&&)=/g, 'logical assignment (Chrome 85)'],
  [/\b\d+_\d+/g, 'numeric separators (Chrome 75)'],
  [/\.replaceAll\(/g, 'String.replaceAll (Chrome 85)'],
  [/\.at\(/g, 'Array/String .at() (Chrome 92)'],
  [/\.findLast(Index)?\(/g, 'findLast (Chrome 97)'],
  [/\bObject\.hasOwn\(/g, 'Object.hasOwn (Chrome 93)'],
  [/\bstructuredClone\(/g, 'structuredClone (Chrome 98)'],
  [/\bPromise\.(allSettled|any)\(/g, 'Promise.allSettled/any (Chrome 76/85)'],
  [/\.replaceChildren\(/g, 'replaceChildren (Chrome 86)'],
  [/\bstatic\s*\{/g, 'static blocks (Chrome 94)'],
  [/\b(dateStyle|timeStyle)\b/g, 'dateStyle/timeStyle (Chrome 76)'],
  [/\bIntl\.(DisplayNames|Segmenter)\b/g, 'Intl.DisplayNames/Segmenter (Chrome 81/87)'],
];

const STYLE_RULES = [
  [/(^|[;{\s])inset\s*:/g, 'inset (Chrome 87)'],
  [/aspect-ratio\s*:/g, 'aspect-ratio (Chrome 88)'],
  [/\b(clamp|min|max)\(/g, 'CSS clamp/min/max (Chrome 79)'],
  [/backdrop-filter/g, 'backdrop-filter (Chrome 76)'],
  [/:(is|where|has|focus-visible)\(?/g, ':is/:where/:has/:focus-visible (Chrome 86+)'],
  [/\b(dvh|svh|lvh|dvw|svw|lvw)\b/g, 'dynamic viewport units (Chrome 108)'],
  [/accent-color/g, 'accent-color (Chrome 93)'],
  [/@container/g, 'container queries (Chrome 105)'],
];

/// A rule's `gap` works in a grid in Chrome 74 but not in a flex box (Chrome 84).
function flexGaps(name, css) {
  const found = [];
  for (const rule of css.matchAll(/[^{}]*\{([^{}]*)\}/g)) {
    const body = rule[1];
    if (/(^|[;\s])(row-|column-)?gap\s*:/.test(body) && !/display\s*:\s*(inline-)?grid/.test(body)) {
      found.push(`${name}:${lineOf(css, rule.index + rule[0].indexOf('{'))} gap outside a grid (flex gap is Chrome 84): ${rule[0].split('{')[0].trim()}`);
    }
  }
  return found;
}

const withoutCssComments = (css) => css.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\n]/g, ' '));
// The page's own <style> and <script>, and its style="" attributes.
const page = readFileSync(new URL('index.html', SCREEN), 'utf8');
const inline = (tag) => [...page.matchAll(new RegExp(`<${tag}[^>]*>([\\s\\S]*?)</${tag}>`, 'g'))].map((m) => m[1]).join('\n');
const attributes = [...page.matchAll(/style="([^"]*)"/g)].map((m) => `x{${m[1]}}`).join('\n');

test('the screen\'s scripts use nothing newer than Chromium 74', () => {
  const scripts = files('.js').concat([{ name: 'index.html <script>', text: inline('script') }]);
  const found = scripts.flatMap(({ name, text }) => findings(name, codeOnly(text), SCRIPT_RULES));
  assert.deepEqual(found, []);
});

test('the screen\'s styles use nothing newer than Chromium 74', () => {
  const sheets = files('.css').concat([
    { name: 'index.html <style>', text: inline('style') },
    { name: 'index.html style=""', text: attributes },
  ]);
  const found = sheets.flatMap(({ name, text }) => {
    const css = withoutCssComments(text);
    return findings(name, css, STYLE_RULES).concat(flexGaps(name, css));
  });
  assert.deepEqual(found, []);
});

test('the reading of scripts tells code from text', () => {
  const read = codeOnly([
    'const a = b ? .5 : c;           // ?. in a comment',
    "const s = 'x?.y ?? z', t = \"a ?? b\";",
    'const u = `${a?.b} and ${`${c ?? d}`}` + `?.`;',
    'const r = /\\?\\.[/?.]x/g.test(s) ? 1 / 2 : /??/;',
    '/* ?? */ const v = w?.x;',
  ].join('\n'));
  assert.deepEqual(findings('t', read, SCRIPT_RULES), [
    't:3 optional chaining (Chrome 80): ?.',
    't:5 optional chaining (Chrome 80): ?.',
    't:3 nullish coalescing (Chrome 80): ??',
  ]);
  assert.deepEqual(flexGaps('t', '.a { display: flex; gap: 4px }\n.b { display: grid; gap: 4px }'), [
    't:1 gap outside a grid (flex gap is Chrome 84): .a',
  ]);
});
