// Client-side wiring for the estrogen demo. Everything runs in the browser:
// CodeMirror for the editor, the estrogen WebAssembly engine for the pass, and
// plain DOM for the tabs, the folder upload, and the copy and download
// controls. There is no server.

import CodeMirror from 'codemirror';
import 'codemirror/mode/javascript/javascript.js';
import 'codemirror/lib/codemirror.css';
import { zipSync, type Zippable } from 'fflate';

import init, { obfuscate } from '../wasm/estrogen.js';

const SAMPLE = `function greet(name) {
	const greeting = 'Hello, ' + name + '!';
	console.log(greeting);
	return greeting;
}

const messages = {
	start: 'starting up',
	done: 'all finished',
};

greet('world');
console.log(messages.start);
console.log(\`today is \${new Date().toDateString()}\`);
`;

const DEFAULT_OUTPUT_FILE_NAME = 'obfuscated.js';
const JS_EXT = /\.(?:js|mjs|cjs)$/i;

function byId<T extends HTMLElement>(id: string): T {
	const el = document.getElementById(id);
	if (!el) throw new Error(`missing #${id}`);
	return el as T;
}

// Editor ------------------------------------------------------------------

const editorArea = byId<HTMLTextAreaElement>('editor');
const editor = CodeMirror.fromTextArea(editorArea, {
	lineNumbers: true,
	mode: 'javascript',
	autofocus: false,
});
editor.setValue(SAMPLE);

const wrapToggle = byId<HTMLInputElement>('wrapToggle');
const vmToggle = byId<HTMLInputElement>('vmToggle');

let ready = false;

// Tabs --------------------------------------------------------------------

const tabs = Array.from(
	document.querySelectorAll<HTMLButtonElement>('[data-tab]'),
);
const panes = Array.from(document.querySelectorAll<HTMLElement>('[data-pane]'));

function selectTab(name: string): void {
	for (const tab of tabs) {
		tab.classList.toggle('active', tab.dataset.tab === name);
	}
	for (const pane of panes) {
		pane.classList.toggle('active', pane.dataset.pane === name);
	}
	if (name === 'code') {
		// CodeMirror needs a refresh once its pane is visible, or it renders blank.
		setTimeout(() => editor.refresh(), 1);
	} else if (name === 'output') {
		// The saved width was applied while this pane was hidden (viewer width 0),
		// so re-clamp it now that the viewer has a real width.
		reclampFilesWidth();
	}
}

for (const tab of tabs) {
	tab.addEventListener('click', () => selectTab(tab.dataset.tab as string));
}

// Output files ------------------------------------------------------------
//
// One list holds what the last run produced: the single pasted file, or every
// file of an uploaded folder. The sidebar lists it, the viewer shows the
// focused file, Download saves that file and Download .zip saves them all.

interface OutputFile {
	path: string;
	bytes: Uint8Array;
	// The file as text, or null when it is not UTF-8 (binary, or holds NUL).
	text: string | null;
	edits: number;
	skipped: boolean;
	// The V3 source map JSON for this file, when the engine produced one. A
	// skipped (passed-through) file has none.
	map?: string;
}

const obfuscateBtn = byId<HTMLButtonElement>('obfuscateBtn');
const outputArea = byId<HTMLTextAreaElement>('output');
const editsCount = byId('editsCount');
const downloadBtn = byId<HTMLButtonElement>('downloadBtn');
const downloadMapBtn = byId<HTMLButtonElement>('downloadMapBtn');
const downloadAllBtn = byId<HTMLButtonElement>('downloadAllBtn');
const seedInput = byId<HTMLInputElement>('seed');
const fileList = byId('fileList');
const viewPath = byId('viewPath');
const unsupported = byId('unsupported');
const copyBtn = byId<HTMLButtonElement>('copyBtn');

const encoder = new TextEncoder();
const utf8 = new TextDecoder('utf-8', { fatal: true });

// The sidebar is an explorer: one row per output file, and under each file the
// engine mapped, a second row for its `<path>.map`. `entries` is that flat,
// rendered list; `active` indexes it.
interface Entry {
	kind: 'file' | 'map';
	file: OutputFile;
	// The row's own path: the file's path, or `<path>.map` for a map row.
	path: string;
}

let outputs: OutputFile[] = [];
let entries: Entry[] = [];
let active = -1;
let zipName = 'estrogen.zip';

function buildEntries(): void {
	entries = [];
	for (const file of outputs) {
		entries.push({ kind: 'file', file, path: file.path });
		if (file.map !== undefined) {
			entries.push({ kind: 'map', file, path: `${file.path}.map` });
		}
	}
}

function anyMap(): boolean {
	return outputs.some((f) => f.map !== undefined);
}

function asText(bytes: Uint8Array): string | null {
	if (bytes.includes(0)) return null;
	try {
		return utf8.decode(bytes);
	} catch {
		return null;
	}
}

function extension(path: string): string {
	const match = /\.([^./]+)$/.exec(path);
	return match ? match[1].toLowerCase() : '';
}

const MIME: Record<string, string> = {
	js: 'text/javascript',
	mjs: 'text/javascript',
	cjs: 'text/javascript',
	html: 'text/html',
	htm: 'text/html',
	css: 'text/css',
	json: 'application/json',
	svg: 'image/svg+xml',
	txt: 'text/plain',
	map: 'application/json',
};

function setOutputs(files: OutputFile[], name: string): void {
	outputs = files.slice().sort((a, b) => a.path.localeCompare(b.path));
	zipName = name;
	buildEntries();
	renderFiles();
	// Open the first file the engine rewrote, else the first entry.
	const first = entries.findIndex((e) => e.kind === 'file' && e.file.edits > 0);
	showEntry(entries.length === 0 ? -1 : Math.max(first, 0));
}

function renderFiles(): void {
	fileList.replaceChildren();
	if (outputs.length === 0) {
		const empty = document.createElement('span');
		empty.className = 'files-empty';
		empty.textContent = 'No output yet';
		fileList.append(empty);
		return;
	}
	entries.forEach((entry, index) => {
		const item = document.createElement('button');
		item.type = 'button';
		item.className = entry.kind === 'map' ? 'file map-row' : 'file';
		item.dataset.index = String(index);
		item.title = entry.path;
		const slash = entry.path.lastIndexOf('/');
		if (slash >= 0) {
			const dir = document.createElement('span');
			dir.className = 'dir';
			dir.textContent = entry.path.slice(0, slash + 1);
			item.append(dir);
		}
		const name = document.createElement('span');
		name.className = 'name';
		name.textContent = entry.path.slice(slash + 1);
		item.append(name);
		if (entry.kind === 'file' && (entry.file.skipped || entry.file.edits > 0)) {
			const badge = document.createElement('span');
			badge.className = entry.file.skipped ? 'badge skip' : 'badge';
			badge.textContent = entry.file.skipped
				? 'skipped'
				: String(entry.file.edits);
			badge.title = entry.file.skipped
				? 'Failed to parse, copied through unchanged'
				: `${entry.file.edits} edits`;
			item.append(badge);
		}
		item.addEventListener('click', () => showEntry(index));
		fileList.append(item);
	});
}

// The source map is shipped minified; show it pretty-printed in the viewer.
function prettyMap(map: string): string {
	try {
		return JSON.stringify(JSON.parse(map), null, 2);
	} catch {
		return map;
	}
}

// The Download .map button is shown whenever the output holds any map, so it is
// pressable even when the focused row is a file with no map, or nothing is
// focused. Its label follows the focused file's extension.
function updateMapButton(): void {
	downloadMapBtn.hidden = !anyMap();
	const entry = entries[active];
	const ext = entry ? extension(entry.file.path) : '';
	downloadMapBtn.textContent = ext ? `Download .${ext}.map` : 'Download .map';
}

function showEntry(index: number): void {
	active = index;
	for (const item of fileList.querySelectorAll<HTMLElement>('.file')) {
		const on = Number(item.dataset.index) === index;
		item.classList.toggle('current', on);
		if (on) item.setAttribute('aria-current', 'true');
		else item.removeAttribute('aria-current');
	}
	const entry = entries[index];
	if (!entry) {
		viewPath.textContent = '';
		outputArea.value = '';
		outputArea.hidden = false;
		unsupported.hidden = true;
		copyBtn.disabled = true;
		downloadBtn.disabled = true;
		downloadBtn.textContent = 'Download .js';
		downloadAllBtn.disabled = true;
		updateMapButton();
		return;
	}
	viewPath.textContent = entry.path;
	if (entry.kind === 'map') {
		outputArea.value = prettyMap(entry.file.map ?? '');
		outputArea.scrollTop = 0;
		outputArea.hidden = false;
		unsupported.hidden = true;
		copyBtn.disabled = false;
		downloadBtn.textContent = 'Download .map';
	} else {
		const file = entry.file;
		const isText = file.text !== null;
		outputArea.value = file.text ?? '';
		outputArea.scrollTop = 0;
		outputArea.hidden = !isText;
		unsupported.hidden = isText;
		copyBtn.disabled = !isText;
		const ext = extension(file.path);
		downloadBtn.textContent = ext ? `Download .${ext}` : 'Download file';
	}
	downloadBtn.disabled = false;
	downloadAllBtn.disabled = false;
	updateMapButton();
}

// Arrow keys move through the file list, Home and End jump to its ends.
fileList.addEventListener('keydown', (event) => {
	if (entries.length === 0) return;
	let next = active;
	if (event.key === 'ArrowDown') next = Math.min(active + 1, entries.length - 1);
	else if (event.key === 'ArrowUp') next = Math.max(active - 1, 0);
	else if (event.key === 'Home') next = 0;
	else if (event.key === 'End') next = entries.length - 1;
	else return;
	event.preventDefault();
	showEntry(next);
	fileList
		.querySelector<HTMLElement>(`.file[data-index="${next}"]`)
		?.focus();
});

// Resizable file list -----------------------------------------------------
//
// The handle between the list and the viewer drags the list's width, kept in
// the --files-w variable on .viewer and remembered across visits. The width
// is clamped so neither side disappears.

const viewer = byId('app').querySelector<HTMLElement>('.viewer');
const filesResize = byId('filesResize');
const FILES_MIN = 120;
const WIDTH_KEY = 'estrogen:files-width';

function clampFilesWidth(px: number): number {
	const width = viewer ? viewer.clientWidth : 0;
	// While the Output pane is hidden the viewer measures 0, so only enforce the
	// minimum; the viewer-relative maximum is applied once it is visible.
	if (width <= 0) return Math.round(Math.max(px, FILES_MIN));
	const max = Math.max(FILES_MIN, width - 160);
	return Math.round(Math.min(Math.max(px, FILES_MIN), max));
}

// Re-apply the current width against the now-visible viewer, so a saved width
// wider than this window gets clamped down.
function reclampFilesWidth(): void {
	if (!viewer) return;
	requestAnimationFrame(() => {
		const current = viewer.querySelector<HTMLElement>('.files')?.offsetWidth;
		if (current) setFilesWidth(current, false);
	});
}

function setFilesWidth(px: number, save: boolean): void {
	if (!viewer) return;
	const width = clampFilesWidth(px);
	viewer.style.setProperty('--files-w', `${width}px`);
	filesResize.setAttribute('aria-valuenow', String(width));
	if (save) {
		try {
			localStorage.setItem(WIDTH_KEY, String(width));
		} catch {
			// Private mode or blocked storage: the width just does not persist.
		}
	}
}

try {
	const saved = Number(localStorage.getItem(WIDTH_KEY));
	if (Number.isFinite(saved) && saved > 0) setFilesWidth(saved, false);
} catch {
	// No stored width; the CSS default stands.
}

filesResize.addEventListener('pointerdown', (event) => {
	if (!viewer || event.button !== 0) return;
	event.preventDefault();
	const startX = event.clientX;
	const startWidth =
		viewer.querySelector<HTMLElement>('.files')?.getBoundingClientRect()
			.width ?? FILES_MIN;
	filesResize.setPointerCapture(event.pointerId);
	viewer.classList.add('resizing');
	const move = (e: PointerEvent) =>
		setFilesWidth(startWidth + (e.clientX - startX), false);
	const up = (e: PointerEvent) => {
		setFilesWidth(startWidth + (e.clientX - startX), true);
		viewer.classList.remove('resizing');
		filesResize.releasePointerCapture(event.pointerId);
		filesResize.removeEventListener('pointermove', move);
		filesResize.removeEventListener('pointerup', up);
		filesResize.removeEventListener('pointercancel', up);
	};
	filesResize.addEventListener('pointermove', move);
	filesResize.addEventListener('pointerup', up);
	filesResize.addEventListener('pointercancel', up);
});

// Keyboard: the handle is a focusable separator; arrows nudge the width.
filesResize.addEventListener('keydown', (event) => {
	if (!viewer) return;
	const current =
		viewer.querySelector<HTMLElement>('.files')?.getBoundingClientRect()
			.width ?? FILES_MIN;
	const step = event.shiftKey ? 48 : 16;
	if (event.key === 'ArrowLeft') setFilesWidth(current - step, true);
	else if (event.key === 'ArrowRight') setFilesWidth(current + step, true);
	else return;
	event.preventDefault();
});

// A window resize can leave the saved width wider than the viewer now allows.
window.addEventListener('resize', () => {
	if (!viewer) return;
	const current = viewer.querySelector<HTMLElement>('.files')?.offsetWidth;
	if (current) setFilesWidth(current, false);
});

// Obfuscate (paste tab) ---------------------------------------------------

// Wrap long lines in both the editor and the output panel, off by default.
wrapToggle.addEventListener('change', () => {
	editor.setOption('lineWrapping', wrapToggle.checked);
	outputArea.classList.toggle('wrap', wrapToggle.checked);
});

function currentSeed(): string | null {
	const seed = seedInput.value.trim();
	return seed === '' ? null : seed;
}

function runObfuscate(): void {
	if (!ready) return;
	try {
		const result = JSON.parse(
			obfuscate(editor.getValue(), currentSeed(), vmToggle.checked),
		);
		const code = result.code as string;
		const n = result.edits as number;
		setOutputs(
			[
				{
					path: DEFAULT_OUTPUT_FILE_NAME,
					bytes: encoder.encode(code),
					text: code,
					edits: n,
					skipped: false,
					map: mapOf(result),
				},
			],
			'estrogen.zip',
		);
		editsCount.textContent = n === 1 ? '1 edit' : `${n} edits`;
		selectTab('output');
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		setOutputs([], 'estrogen.zip');
		editsCount.textContent = `error: ${message}`;
		selectTab('output');
	}
}

obfuscateBtn.addEventListener('click', runObfuscate);

// Reset -------------------------------------------------------------------

byId('resetBtn').addEventListener('click', () => {
	editor.setValue(SAMPLE);
	setOutputs([], 'estrogen.zip');
	editsCount.textContent = '';
	uploadStatus.classList.remove('show');
	downloadZipBtn.disabled = true;
	selectTab('code');
});

// Copy output -------------------------------------------------------------

let copyTimer: number | undefined;

copyBtn.addEventListener('click', async () => {
	if (!outputArea.value || outputArea.hidden) return;
	try {
		await navigator.clipboard.writeText(outputArea.value);
	} catch {
		// Fall back to the selection path when the clipboard API is blocked.
		outputArea.focus();
		outputArea.select();
		document.execCommand('copy');
	}
	copyBtn.textContent = 'Copied';
	copyBtn.classList.add('done');
	window.clearTimeout(copyTimer);
	copyTimer = window.setTimeout(() => {
		copyBtn.textContent = 'Copy';
		copyBtn.classList.remove('done');
	}, 1400);
});

// Downloads ---------------------------------------------------------------

function download(data: BlobPart, name: string, type: string): void {
	const url = URL.createObjectURL(new Blob([data], { type }));
	const a = document.createElement('a');
	a.href = url;
	a.download = name;
	document.body.appendChild(a);
	a.click();
	a.remove();
	// Revoke after the click has started the download.
	setTimeout(() => URL.revokeObjectURL(url), 1000);
}

function basename(path: string): string {
	return path.slice(path.lastIndexOf('/') + 1);
}

downloadBtn.addEventListener('click', () => {
	const entry = entries[active];
	if (!entry) return;
	if (entry.kind === 'map') {
		download(entry.file.map ?? '', basename(entry.path), 'application/json');
		return;
	}
	download(
		entry.file.bytes as BlobPart,
		basename(entry.file.path),
		MIME[extension(entry.file.path)] ?? 'application/octet-stream',
	);
});

// The engine returns the source map on the obfuscate result. Accept it as a
// JSON string or an already-parsed object, so the exact WASM shape does not
// matter to the download: a V3 map is downloaded as <file>.map.
function mapOf(result: { map?: unknown }): string | undefined {
	const map = result.map;
	if (typeof map === 'string') return map.length > 0 ? map : undefined;
	if (map && typeof map === 'object') return JSON.stringify(map);
	return undefined;
}

downloadMapBtn.addEventListener('click', () => {
	// The focused entry's map, or the first mapped file when the focus has none
	// or nothing is focused, so the button always does something useful.
	const entry = entries[active];
	const file =
		entry && entry.file.map !== undefined
			? entry.file
			: outputs.find((f) => f.map !== undefined);
	if (!file?.map) return;
	download(file.map, `${basename(file.path)}.map`, 'application/json');
});

function downloadZip(): void {
	if (outputs.length === 0) return;
	const zip: Zippable = {};
	for (const file of outputs) {
		zip[file.path] = file.bytes;
		// The whole codebase, including a `<path>.map` next to each mapped file.
		if (file.map !== undefined) zip[`${file.path}.map`] = encoder.encode(file.map);
	}
	download(zipSync(zip, { level: 6 }) as BlobPart, zipName, 'application/zip');
}

downloadAllBtn.addEventListener('click', downloadZip);

// Folder upload -----------------------------------------------------------

const dropZone = byId('dropZone');
const folderInput = byId<HTMLInputElement>('folderInput');
const uploadStatus = byId('uploadStatus');
const progressBar = byId<HTMLProgressElement>('progressBar');
const progressText = byId('progressText');
const skips = byId('skips');
const downloadZipBtn = byId<HTMLButtonElement>('downloadZipBtn');

interface NamedFile {
	path: string;
	file: File;
}

async function readBytes(file: File): Promise<Uint8Array> {
	return new Uint8Array(await file.arrayBuffer());
}

// Walk a dropped directory tree via the webkit entry API so a dropped folder
// behaves like the folder picker. Flat file drops fall back to their names.
function readEntries(reader: {
	readEntries(
		cb: (e: FileSystemEntry[]) => void,
		err: (e: unknown) => void,
	): void;
}): Promise<FileSystemEntry[]> {
	return new Promise((resolve, reject) => reader.readEntries(resolve, reject));
}

async function walkEntry(
	entry: FileSystemEntry,
	out: NamedFile[],
): Promise<void> {
	if (entry.isFile) {
		const file = await new Promise<File>((resolve, reject) =>
			(entry as FileSystemFileEntry).file(resolve, reject),
		);
		out.push({ path: entry.fullPath.replace(/^\//, ''), file });
		return;
	}
	const dirReader = (entry as FileSystemDirectoryEntry).createReader();
	let batch = await readEntries(dirReader);
	while (batch.length > 0) {
		for (const child of batch) await walkEntry(child, out);
		batch = await readEntries(dirReader);
	}
}

async function collectFromDataTransfer(dt: DataTransfer): Promise<NamedFile[]> {
	const items = Array.from(dt.items).filter((i) => i.kind === 'file');
	const entries = items
		.map((i) => (i.webkitGetAsEntry ? i.webkitGetAsEntry() : null))
		.filter((e): e is FileSystemEntry => e != null);
	if (entries.length > 0) {
		const out: NamedFile[] = [];
		for (const entry of entries) await walkEntry(entry, out);
		return out;
	}
	// No entry API: use the flat file list by name.
	return Array.from(dt.files).map((file) => ({ path: file.name, file }));
}

function collectFromInput(input: HTMLInputElement): NamedFile[] {
	return Array.from(input.files ?? []).map((file) => ({
		path: file.webkitRelativePath || file.name,
		file,
	}));
}

async function runFolder(files: NamedFile[]): Promise<void> {
	if (!ready) return;
	if (files.length === 0) return;

	uploadStatus.classList.add('show');
	downloadZipBtn.disabled = true;
	skips.textContent = '';
	progressBar.value = 0;

	const seed = currentSeed();
	const results: OutputFile[] = [];
	const skipped: string[] = [];
	let edits = 0;
	let obfuscated = 0;
	let done = 0;

	for (const { path, file } of files) {
		const original = await readBytes(file);
		let out: OutputFile = {
			path,
			bytes: original,
			text: null,
			edits: 0,
			skipped: false,
		};
		const source = JS_EXT.test(path) ? asText(original) : null;
		if (source !== null) {
			try {
				const result = JSON.parse(obfuscate(source, seed, vmToggle.checked));
				const code = result.code as string;
				const count = (result.edits as number) ?? 0;
				out = {
					path,
					bytes: encoder.encode(code),
					text: code,
					edits: count,
					skipped: false,
					map: mapOf(result),
				};
				edits += count;
				if (count > 0) obfuscated += 1;
			} catch (err) {
				// A file that fails to parse passes through unchanged, noted as a skip.
				const message = err instanceof Error ? err.message : String(err);
				out = { ...out, text: source, skipped: true };
				skipped.push(`${path}: ${message}`);
			}
		} else {
			out.text = asText(original);
		}
		results.push(out);
		done += 1;
		progressBar.value = Math.round((done / files.length) * 100);
		progressText.textContent = `${done} / ${files.length} files, ${obfuscated} obfuscated, ${edits} edits`;
		if (done % 20 === 0) await new Promise((r) => setTimeout(r, 0));
	}

	if (skipped.length > 0) {
		skips.textContent = `Passed through unchanged (parse error):\n${skipped.join('\n')}`;
	}
	const summary = `${files.length} files, ${obfuscated} obfuscated, ${edits} edits, ${skipped.length} skipped`;
	progressText.textContent = summary;

	const top = files[0]?.path.split('/')[0];
	setOutputs(
		results,
		top && top !== files[0]?.file.name ? `${top}.estrogen.zip` : 'estrogen.zip',
	);
	editsCount.textContent = summary;
	downloadZipBtn.disabled = false;
	selectTab('output');
}

dropZone.addEventListener('click', () => folderInput.click());
folderInput.addEventListener('change', () => {
	void runFolder(collectFromInput(folderInput));
	folderInput.value = '';
});
dropZone.addEventListener('dragover', (event) => {
	event.preventDefault();
	dropZone.classList.add('dragover');
});
dropZone.addEventListener('dragleave', () =>
	dropZone.classList.remove('dragover'),
);
dropZone.addEventListener('drop', (event) => {
	event.preventDefault();
	dropZone.classList.remove('dragover');
	if (event.dataTransfer)
		void collectFromDataTransfer(event.dataTransfer).then(runFolder);
});

downloadZipBtn.addEventListener('click', downloadZip);

setOutputs([], 'estrogen.zip');

// Engine ------------------------------------------------------------------

init()
	.then(() => {
		ready = true;
	})
	.catch((err) => {
		const message = err instanceof Error ? err.message : String(err);
		obfuscateBtn.disabled = true;
		obfuscateBtn.textContent = 'engine failed';
		editsCount.textContent = message;
		console.error(err);
	});
