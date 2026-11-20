// Client-side wiring for the estrogen demo. Everything runs in the browser:
// CodeMirror for the editor, the estrogen WebAssembly engine for the pass, and
// plain DOM for the tabs, upload, download and evaluate controls. There is no
// server.

import CodeMirror from 'codemirror';
import 'codemirror/mode/javascript/javascript.js';
import 'codemirror/lib/codemirror.css';

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

let outputFileName = DEFAULT_OUTPUT_FILE_NAME;

// Tabs --------------------------------------------------------------------

const menu = byId('tabMenu');
const items = Array.from(
	menu.querySelectorAll<HTMLAnchorElement>('[data-tab]'),
);
const panes = Array.from(
	document.querySelectorAll<HTMLElement>('[data-pane]'),
);

function selectTab(name: string): void {
	for (const item of items) {
		item.classList.toggle('active', item.dataset.tab === name);
	}
	for (const pane of panes) {
		pane.classList.toggle('active', pane.dataset.pane === name);
	}
	if (name === 'code') {
		// CodeMirror needs a refresh once its pane is visible, or it renders blank.
		setTimeout(() => editor.refresh(), 1);
	}
}

for (const item of items) {
	item.addEventListener('click', (event) => {
		event.preventDefault();
		selectTab(item.dataset.tab as string);
	});
}

// Obfuscate ---------------------------------------------------------------

const obfuscateBtn = byId<HTMLButtonElement>('obfuscateBtn');
const outputArea = byId<HTMLTextAreaElement>('output');
const editsCount = byId('editsCount');
const downloadBtn = byId<HTMLButtonElement>('downloadBtn');
const seedInput = byId<HTMLInputElement>('seed');

let ready = false;

function runObfuscate(): void {
	if (!ready) return;
	const source = editor.getValue();
	const seed = seedInput.value.trim();
	try {
		const result = JSON.parse(obfuscate(source, seed === '' ? null : seed));
		outputArea.value = result.code;
		const n = result.edits as number;
		editsCount.textContent =
			n === 1 ? '1 literal pooled' : `${n} literals pooled`;
		downloadBtn.disabled = false;
		selectTab('output');
		if (evaluateChk.checked) runEvaluate(result.code);
	} catch (err) {
		// Keep the previous output; show the engine's message.
		const message = err instanceof Error ? err.message : String(err);
		editsCount.textContent = `error: ${message}`;
	}
}

obfuscateBtn.addEventListener('click', runObfuscate);

// Upload ------------------------------------------------------------------

const dropZone = byId('dropZone');
const fileInput = byId<HTMLInputElement>('fileInput');

function loadFile(file: File): void {
	const reader = new FileReader();
	reader.onload = (event) => {
		editor.setValue(String(event.target?.result ?? ''));
		outputFileName = file.name || DEFAULT_OUTPUT_FILE_NAME;
		selectTab('code');
	};
	reader.readAsText(file);
}

dropZone.addEventListener('click', () => fileInput.click());
fileInput.addEventListener('change', () => {
	const file = fileInput.files?.[0];
	if (file) loadFile(file);
	fileInput.value = '';
});
dropZone.addEventListener('dragover', (event) => {
	event.preventDefault();
	dropZone.classList.add('dragover');
});
dropZone.addEventListener('dragleave', () => {
	dropZone.classList.remove('dragover');
});
dropZone.addEventListener('drop', (event) => {
	event.preventDefault();
	dropZone.classList.remove('dragover');
	const file = event.dataTransfer?.files?.[0];
	if (file) loadFile(file);
});

// Download ----------------------------------------------------------------

downloadBtn.addEventListener('click', () => {
	const blob = new Blob([outputArea.value], { type: 'text/javascript' });
	const url = URL.createObjectURL(blob);
	const a = document.createElement('a');
	a.href = url;
	a.download = outputFileName || DEFAULT_OUTPUT_FILE_NAME;
	document.body.appendChild(a);
	a.click();
	a.remove();
	URL.revokeObjectURL(url);
});

// Output selects on focus, matching the reference UI.
outputArea.addEventListener('focus', () => outputArea.select());

// Evaluate ----------------------------------------------------------------

const evaluateChk = byId<HTMLInputElement>('evaluateChk');
const evaluatedCode = byId('evaluatedCode');

// Ported from CodeContainer.evaluate: run the output with a captured console.
function runEvaluate(code: string): void {
	const buffer: string[] = [];
	const capturingConsole = Object.create(console);
	for (const key of ['error', 'log', 'info', 'debug'] as const) {
		capturingConsole[key] = function (...args: unknown[]) {
			Function.prototype.apply.call(console[key], console, args);
			buffer.push(args.map((a) => String(a)).join('\n'));
		};
	}
	try {
		new Function('console', code)(capturingConsole);
	} catch (err) {
		buffer.push(err instanceof Error ? err.message : String(err));
	}
	evaluatedCode.textContent = buffer.join('\n');
}

evaluateChk.addEventListener('change', () => {
	evaluatedCode.hidden = !evaluateChk.checked;
	if (evaluateChk.checked) runEvaluate(outputArea.value);
});

// Sample ------------------------------------------------------------------

byId('sampleBtn').addEventListener('click', () => {
	editor.setValue(SAMPLE);
	selectTab('code');
});

// Engine ------------------------------------------------------------------

const engineVersion = byId('engineVersion');

init()
	.then(() => {
		ready = true;
		engineVersion.textContent = `engine v${version()}, running as WebAssembly`;
	})
	.catch((err) => {
		engineVersion.textContent = `engine failed to load: ${
			err instanceof Error ? err.message : String(err)
		}`;
	});
