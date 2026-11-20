// Client-side wiring for the estrogen demo. Everything runs in the browser:
// CodeMirror for the editor, the estrogen WebAssembly engine for the pass, and
// plain DOM for the tabs, the folder upload, and the copy and download
// controls. There is no server.

import CodeMirror from 'codemirror';
import 'codemirror/mode/javascript/javascript.js';
import 'codemirror/lib/codemirror.css';
import { zipSync, type Zippable } from 'fflate';

import init, { obfuscate, version } from '../wasm/estrogen.js';

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

let outputFileName = DEFAULT_OUTPUT_FILE_NAME;
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
	}
}

for (const tab of tabs) {
	tab.addEventListener('click', () => selectTab(tab.dataset.tab as string));
}

// Obfuscate (paste tab) ---------------------------------------------------

const obfuscateBtn = byId<HTMLButtonElement>('obfuscateBtn');
const outputArea = byId<HTMLTextAreaElement>('output');
const editsCount = byId('editsCount');
const downloadBtn = byId<HTMLButtonElement>('downloadBtn');
const seedInput = byId<HTMLInputElement>('seed');

function currentSeed(): string | null {
	const seed = seedInput.value.trim();
	return seed === '' ? null : seed;
}

function runObfuscate(): void {
	if (!ready) return;
	try {
		const result = JSON.parse(obfuscate(editor.getValue(), currentSeed()));
		outputArea.value = result.code;
		const n = result.edits as number;
		editsCount.textContent =
			n === 1 ? '1 literal pooled' : `${n} literals pooled`;
		downloadBtn.disabled = false;
		selectTab('output');
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		editsCount.textContent = `error: ${message}`;
		selectTab('output');
	}
}

obfuscateBtn.addEventListener('click', runObfuscate);

// Reset -------------------------------------------------------------------

byId('resetBtn').addEventListener('click', () => {
	editor.setValue(SAMPLE);
	outputArea.value = '';
	editsCount.textContent = '';
	downloadBtn.disabled = true;
	outputFileName = DEFAULT_OUTPUT_FILE_NAME;
	selectTab('code');
});

// Copy output -------------------------------------------------------------

const copyBtn = byId<HTMLButtonElement>('copyBtn');
let copyTimer: number | undefined;

copyBtn.addEventListener('click', async () => {
	if (!outputArea.value) return;
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

// Download single output --------------------------------------------------

function download(data: BlobPart, name: string, type: string): void {
	const url = URL.createObjectURL(new Blob([data], { type }));
	const a = document.createElement('a');
	a.href = url;
	a.download = name;
	document.body.appendChild(a);
	a.click();
	a.remove();
	URL.revokeObjectURL(url);
}

downloadBtn.addEventListener('click', () => {
	download(outputArea.value, outputFileName, 'text/javascript');
});

// Folder upload -> zip ----------------------------------------------------

const dropZone = byId('dropZone');
const folderInput = byId<HTMLInputElement>('folderInput');
const uploadStatus = byId('uploadStatus');
const progressFill = byId('progressFill');
const progressText = byId('progressText');
const skips = byId('skips');
const downloadZipBtn = byId<HTMLButtonElement>('downloadZipBtn');

let zipBlobUrl: string | null = null;
let zipName = 'obfuscated.zip';

const encoder = new TextEncoder();

interface NamedFile {
	path: string;
	file: File;
}

async function readBytes(file: File): Promise<Uint8Array> {
	return new Uint8Array(await file.arrayBuffer());
}

async function readText(file: File): Promise<string> {
	return await file.text();
}

// Walk a dropped directory tree via the webkit entry API so a dropped folder
// behaves like the folder picker. Flat file drops fall back to their names.
function readEntries(
	reader: {
		readEntries(cb: (e: FileSystemEntry[]) => void, err: (e: unknown) => void): void;
	},
): Promise<FileSystemEntry[]> {
	return new Promise((resolve, reject) => reader.readEntries(resolve, reject));
}

async function walkEntry(entry: FileSystemEntry, out: NamedFile[]): Promise<void> {
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
	if (zipBlobUrl) {
		URL.revokeObjectURL(zipBlobUrl);
		zipBlobUrl = null;
	}

	const seed = currentSeed();
	const zip: Zippable = {};
	const skipped: string[] = [];
	let pooled = 0;
	let obfuscated = 0;
	let done = 0;

	for (const { path, file } of files) {
		if (JS_EXT.test(path)) {
			try {
				const result = JSON.parse(obfuscate(await readText(file), seed));
				zip[path] = encoder.encode(result.code);
				pooled += (result.edits as number) ?? 0;
				obfuscated += 1;
			} catch (err) {
				// A file that fails to parse passes through unchanged, noted as a skip.
				const message = err instanceof Error ? err.message : String(err);
				zip[path] = await readBytes(file);
				skipped.push(`${path}: ${message}`);
			}
		} else {
			zip[path] = await readBytes(file);
		}
		done += 1;
		const pct = Math.round((done / files.length) * 100);
		progressFill.style.width = `${pct}%`;
		progressText.textContent = `${done} / ${files.length} files, ${obfuscated} obfuscated, ${pooled} literals pooled`;
		if (done % 20 === 0) await new Promise((r) => setTimeout(r, 0));
	}

	if (skipped.length > 0) {
		skips.textContent = `Passed through unchanged (parse error):\n${skipped.join('\n')}`;
	}
	progressText.textContent = `${files.length} files, ${obfuscated} obfuscated, ${pooled} literals pooled, ${skipped.length} skipped`;

	const bytes = zipSync(zip, { level: 6 });
	zipBlobUrl = URL.createObjectURL(
		new Blob([bytes], { type: 'application/zip' }),
	);
	const top = files[0]?.path.split('/')[0];
	zipName = top && top !== files[0]?.file.name ? `${top}.estrogen.zip` : 'estrogen.zip';
	downloadZipBtn.disabled = false;
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
dropZone.addEventListener('dragleave', () => dropZone.classList.remove('dragover'));
dropZone.addEventListener('drop', (event) => {
	event.preventDefault();
	dropZone.classList.remove('dragover');
	if (event.dataTransfer) void collectFromDataTransfer(event.dataTransfer).then(runFolder);
});

downloadZipBtn.addEventListener('click', () => {
	if (!zipBlobUrl) return;
	const a = document.createElement('a');
	a.href = zipBlobUrl;
	a.download = zipName;
	document.body.appendChild(a);
	a.click();
	a.remove();
});

// Engine ------------------------------------------------------------------

const engineVersion = byId('engineVersion');
const engineNote = byId('engineNote');

init()
	.then(() => {
		ready = true;
		const v = version();
		engineVersion.textContent = `engine v${v}`;
		engineNote.textContent = `v${v}, WebAssembly`;
	})
	.catch((err) => {
		const message = err instanceof Error ? err.message : String(err);
		engineVersion.textContent = `engine failed to load`;
		engineNote.textContent = message;
	});
