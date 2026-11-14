<script lang="ts">
	/**
	 * A `matrix` view as 3-D bars, height and color the value: one instanced mesh whatever the mode
	 * count, viridis pinned to
	 * the view's range, frequency labels along the two edges thinned as the grid's are, every bar
	 * outlined in black up to `OUTLINE_LIMIT` a side. The marks match the flat grid's: picked pairs
	 * get a blue outline, the active (animated) pair the overlay's orange, committed matches a red
	 * checker on every face. Clicking a bar picks it as clicking a cell does; the modifier extends.
	 * The camera is placed when the comparison changes and at no other time.
	 */
	import { onDestroy, onMount } from 'svelte';
	import {
		AmbientLight,
		BoxGeometry,
		BufferAttribute,
		BufferGeometry,
		CanvasTexture,
		Color,
		DirectionalLight,
		InstancedMesh,
		LineBasicMaterial,
		LineSegments,
		Mesh,
		MeshBasicMaterial,
		MeshLambertMaterial,
		NearestFilter,
		Object3D,
		PerspectiveCamera,
		Raycaster,
		Scene,
		Vector2,
		Vector3,
		WebGLRenderer
	} from 'three';
	import { TrackballControls } from './trackball';
	import type { MatrixView } from '$lib/protocol/types';
	import { theme } from '$lib/theme/theme.svelte';
	import { colormap, colormapGradient } from './colormap';
	import { formatNumber } from './format';
	import {
		barHeight,
		CHECKER,
		EDGE_LIFT,
		FOOTPRINT,
		fullHeight,
		homeCamera,
		MARK_LIFT,
		MARK_RED,
		named,
		outlineSegments,
		OUTLINE_LIMIT,
		pairOf,
		PICK_BLUE
	} from './macBars';
	import { cellText } from './matrix';

	interface Props {
		view: MatrixView;
		showTitle?: boolean;
		/** The pair the linked shape view animates. */
		selected?: { row: number; column: number } | null;
		/** Every picked pair; the selected one is among them. */
		picks?: { row: number; column: number }[];
		onselect?: (cell: { row: number; column: number }, extend?: boolean) => void;
	}

	let { view, showTitle = true, selected = null, picks = [], onselect }: Props = $props();

	/** The overlay's orange: the active pair reads as the pair the shape view draws. */
	const ACTIVE = '#e69f00';

	const ROWS = $derived(view.rows.ticks.length);
	const COLUMNS = $derived(view.columns.ticks.length);
	const [low, high] = $derived(view.value_range);
	const legendTicks = $derived([low, (low + high) / 2, high].map((t) => String(+t.toFixed(2))));

	let canvasHost: HTMLDivElement;
	let overlay: HTMLDivElement;
	let renderer: WebGLRenderer | null = null;
	let scene: Scene;
	let camera: PerspectiveCamera;
	let controls: TrackballControls;
	let bars: InstancedMesh | null = null;
	let barGeometry: BoxGeometry | null = null;
	let barMaterial: MeshLambertMaterial;
	let edges: LineSegments | null = null;
	let pickLines: LineSegments | null = null;
	let activeLines: LineSegments | null = null;
	let matchedMeshes: Mesh[] = [];
	let checker: CanvasTexture | null = null;
	let axisLine: LineSegments | null = null;
	const lineMaterials: LineBasicMaterial[] = [];
	let raf = 0;
	let dirty = true;
	let resizeObs: ResizeObserver | null = null;
	let failure = $state<string | null>(null);
	let hover = $state<{ row: number; column: number; sx: number; sy: number } | null>(null);
	/** What the camera was placed for; a different comparison starts framed whole. */
	let placedFor = '';
	const raycaster = new Raycaster();
	const pointer = new Vector2();
	const probe = new Vector3();
	const dummy = new Object3D();

	function value(row: number, column: number): number {
		return view.values[row * COLUMNS + column];
	}

	function height(row: number, column: number): number {
		const span = high - low || 1;
		return barHeight((value(row, column) - low) / span, ROWS, COLUMNS);
	}

	const namedRows = $derived(named(ROWS));
	const namedColumns = $derived(named(COLUMNS));
	const tall = $derived(fullHeight(ROWS, COLUMNS));

	function lines(segments: Float32Array, color: string, lift: number): LineSegments {
		void lift;
		const g = new BufferGeometry();
		g.setAttribute('position', new BufferAttribute(segments, 3));
		const m = new LineBasicMaterial({ color: new Color(color) });
		lineMaterials.push(m);
		const l = new LineSegments(g, m);
		l.frustumCulled = false;
		return l;
	}

	function drop(obj: LineSegments | Mesh | null) {
		if (!obj) return;
		scene.remove(obj);
		obj.geometry.dispose();
	}

	/** The checker a committed match wears: red squares and clear ones, nearest-sampled so the squares stay square. */
	function checkerTexture(): CanvasTexture {
		if (checker) return checker;
		const c = document.createElement('canvas');
		c.width = c.height = CHECKER * 8;
		const ctx = c.getContext('2d')!;
		ctx.fillStyle = MARK_RED;
		for (let i = 0; i < CHECKER; i++) {
			for (let j = 0; j < CHECKER; j++) if ((i + j) % 2 === 0) ctx.fillRect(i * 8, j * 8, 8, 8);
		}
		checker = new CanvasTexture(c);
		checker.magFilter = NearestFilter;
		checker.minFilter = NearestFilter;
		return checker;
	}

	/** The bars themselves: one instanced box, each instance placed in its cell and scaled to its height. */
	function build() {
		if (!renderer) return;
		if (bars) {
			scene.remove(bars);
			bars.dispose();
			barGeometry?.dispose();
		}
		drop(edges);
		edges = null;
		drop(axisLine);
		const margin = (1 - FOOTPRINT) / 2;
		barGeometry = new BoxGeometry(FOOTPRINT, FOOTPRINT, 1).translate(0, 0, 0.5);
		bars = new InstancedMesh(barGeometry, barMaterial, ROWS * COLUMNS);
		const rgb: [number, number, number] = [0, 0, 0];
		const span = high - low || 1;
		const color = new Color();
		for (let r = 0; r < ROWS; r++) {
			for (let c = 0; c < COLUMNS; c++) {
				const i = r * COLUMNS + c;
				dummy.position.set(c + margin + FOOTPRINT / 2, r + margin + FOOTPRINT / 2, 0);
				dummy.scale.set(1, 1, height(r, c));
				dummy.updateMatrix();
				bars.setMatrixAt(i, dummy.matrix);
				colormap((value(r, c) - low) / span, rgb);
				bars.setColorAt(i, color.setRGB(rgb[0], rgb[1], rgb[2]));
			}
		}
		bars.instanceMatrix.needsUpdate = true;
		if (bars.instanceColor) bars.instanceColor.needsUpdate = true;
		bars.frustumCulled = false;
		scene.add(bars);
		if (Math.max(ROWS, COLUMNS) <= OUTLINE_LIMIT) {
			const all: { row: number; column: number }[] = [];
			for (let r = 0; r < ROWS; r++) for (let c = 0; c < COLUMNS; c++) all.push({ row: r, column: c });
			edges = lines(outlineSegments(all, height, EDGE_LIFT), '#000000', EDGE_LIFT);
			scene.add(edges);
		}
		// the one axis with a number worth reading: the value, 0 to 1, at the near left corner
		axisLine = lines(new Float32Array([0, 0, 0, 0, 0, tall, 0, 0, 0, COLUMNS, 0, 0, 0, 0, 0, 0, ROWS, 0]), '#8b949e', 0);
		scene.add(axisLine);
		const key = `${view.title}\u0000${ROWS}\u0000${COLUMNS}`;
		if (placedFor !== key) {
			const home = homeCamera(ROWS, COLUMNS);
			camera.position.fromArray(home.position);
			camera.up.set(0, 0, 1);
			controls.target.fromArray(home.target);
			controls.update();
			placedFor = key;
		}
		mark();
		dirty = true;
	}

	/** The comparison's marks: picks in blue, the active pair in orange, committed matches checkered. */
	function mark() {
		if (!renderer) return;
		drop(pickLines);
		drop(activeLines);
		for (const m of matchedMeshes) drop(m);
		matchedMeshes = [];
		const inside = picks.filter((p) => p.row < ROWS && p.column < COLUMNS);
		pickLines = inside.length ? lines(outlineSegments(inside, height), PICK_BLUE, MARK_LIFT) : null;
		if (pickLines) scene.add(pickLines);
		const active = selected && selected.row < ROWS && selected.column < COLUMNS ? [selected] : [];
		activeLines = active.length
			? lines(new Float32Array([...outlineSegments(active, height, MARK_LIFT), ...outlineSegments(active, height, MARK_LIFT * 2)]), ACTIVE, MARK_LIFT)
			: null;
		if (activeLines) scene.add(activeLines);
		const matched = (view.matched ?? []).filter(([r, c]) => r < ROWS && c < COLUMNS);
		if (matched.length) {
			const map = checkerTexture();
			const material = new MeshBasicMaterial({ map, transparent: true, alphaTest: 0.5, depthWrite: false });
			const margin = (1 - FOOTPRINT) / 2;
			for (const [r, c] of matched) {
				const h = height(r, c) + MARK_LIFT;
				const g = new BoxGeometry(FOOTPRINT + 2 * MARK_LIFT, FOOTPRINT + 2 * MARK_LIFT, h).translate(0, 0, h / 2);
				const mesh = new Mesh(g, material);
				mesh.position.set(c + margin + FOOTPRINT / 2, r + margin + FOOTPRINT / 2, 0);
				scene.add(mesh);
				matchedMeshes.push(mesh);
			}
		}
		dirty = true;
	}

	function init() {
		try {
			// the drawing is kept after each frame so a printed report can capture the canvas
			renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: 'high-performance', preserveDrawingBuffer: true });
		} catch {
			failure = 'The bars need WebGL, which is not available in this browser.';
			return;
		}
		renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
		renderer.setClearColor(0x000000, 0);
		canvasHost.appendChild(renderer.domElement);
		renderer.domElement.addEventListener('webglcontextlost', (e) => {
			e.preventDefault();
			failure = 'The graphics context was lost. Switch workspaces and back to rebuild the view.';
		});
		scene = new Scene();
		camera = new PerspectiveCamera(30, 1, 0.05, 1000);
		camera.up.set(0, 0, 1);
		scene.add(new AmbientLight(0xffffff, 0.8));
		const sun = new DirectionalLight(0xffffff, 1.1);
		sun.position.set(-3, -5, 8);
		scene.add(sun);
		barMaterial = new MeshLambertMaterial();
		controls = new TrackballControls(camera, renderer.domElement);
		controls.zoomSpeed = 0.9;
		controls.addEventListener('change', () => (dirty = true));
		resizeObs = new ResizeObserver(resize);
		resizeObs.observe(canvasHost);
		resize();
		build();
		raf = requestAnimationFrame(loop);
	}

	function resize() {
		if (!renderer) return;
		const w = canvasHost.clientWidth;
		const h = canvasHost.clientHeight;
		if (!w || !h) return;
		renderer.setSize(w, h, false);
		camera.aspect = w / h;
		camera.updateProjectionMatrix();
		dirty = true;
	}

	/** Pin each HTML label over its point on the stage. */
	function placeLabels() {
		if (!overlay) return;
		const w = canvasHost.clientWidth;
		const h = canvasHost.clientHeight;
		for (const el of overlay.querySelectorAll<HTMLElement>('[data-at]')) {
			const [x, y, z] = el.dataset.at!.split(',').map(Number);
			probe.set(x, y, z).project(camera);
			el.style.visibility = probe.z < 1 ? 'visible' : 'hidden';
			el.style.transform = `translate(${((probe.x + 1) / 2) * w}px, ${((1 - probe.y) / 2) * h}px)`;
		}
	}

	function loop() {
		raf = requestAnimationFrame(loop);
		const moved = controls.update();
		if (!dirty && !moved) return;
		dirty = false;
		if (!renderer) return;
		renderer.render(scene, camera);
		placeLabels();
	}

	function barAt(e: PointerEvent | MouseEvent): { row: number; column: number } | null {
		if (!bars) return null;
		const box = canvasHost.getBoundingClientRect();
		pointer.set(((e.clientX - box.left) / box.width) * 2 - 1, -((e.clientY - box.top) / box.height) * 2 + 1);
		raycaster.setFromCamera(pointer, camera);
		const hit = raycaster.intersectObject(bars, false)[0];
		return hit && hit.instanceId !== undefined ? pairOf(hit.instanceId, COLUMNS) : null;
	}

	function onMove(e: PointerEvent) {
		const at = barAt(e);
		if (!at) {
			hover = null;
			return;
		}
		const box = canvasHost.getBoundingClientRect();
		hover = { ...at, sx: e.clientX - box.left, sy: e.clientY - box.top };
	}

	let pressedAt: [number, number] | null = null;
	function onDown(e: PointerEvent) {
		pressedAt = [e.clientX, e.clientY];
	}
	function onUp(e: PointerEvent) {
		// a drag orbits; only a click picks
		if (!onselect || !pressedAt || Math.hypot(e.clientX - pressedAt[0], e.clientY - pressedAt[1]) > 4) return;
		const at = barAt(e);
		if (at) onselect(at, e.metaKey || e.ctrlKey || e.shiftKey);
	}

	onMount(init);

	$effect(() => {
		void view;
		void theme.version;
		if (renderer) build();
	});

	$effect(() => {
		void picks;
		void selected;
		if (renderer && bars) mark();
	});

	onDestroy(() => {
		cancelAnimationFrame(raf);
		resizeObs?.disconnect();
		if (!renderer) return;
		controls.dispose();
		bars?.dispose();
		barGeometry?.dispose();
		barMaterial.dispose();
		for (const l of [edges, pickLines, activeLines, axisLine]) l?.geometry.dispose();
		for (const m of matchedMeshes) {
			m.geometry.dispose();
			(m.material as MeshBasicMaterial).dispose();
		}
		for (const m of lineMaterials) m.dispose();
		checker?.dispose();
		renderer.dispose();
	});
</script>

<section class="bars" data-testid="view-mac-bars" aria-label={view.title}>
	{#if showTitle}<h3 class="title">{view.title}</h3>{/if}
	<div class="stage">
		<div
			class="canvas"
			data-testid="mac-bars-canvas"
			bind:this={canvasHost}
			role={onselect ? 'button' : undefined}
			aria-label="{view.value_label} of {ROWS} {view.rows.label} rows by {COLUMNS} {view.columns.label} columns as bars{onselect ? '; click a bar to pick its pair' : ''}"
			onpointermove={onMove}
			onpointerleave={() => (hover = null)}
			onpointerdown={onDown}
			onpointerup={onUp}
		></div>
		<div class="overlay" bind:this={overlay} aria-hidden="true">
			{#each namedRows as r (r)}
				<span class="tick num row" data-at="-0.35,{r + 0.5},0">{view.rows.ticks[r]}</span>
			{/each}
			{#each namedColumns as c (c)}
				<span class="tick num col" data-at="{c + 0.5},-0.35,0">{view.columns.ticks[c]}</span>
			{/each}
			<span class="tick num" data-at="-0.2,-0.2,0">{legendTicks[0]}</span>
			<span class="tick num" data-at="-0.2,-0.2,{tall}">{legendTicks[2]}</span>
			<span class="tick name" data-at="-0.2,-0.2,{tall * 1.12}">{view.value_label}</span>
			<span class="tick name" data-at="{COLUMNS / 2},-1.1,0">{view.columns.label}</span>
			<span class="tick name" data-at="-1.1,{ROWS / 2},0">{view.rows.label}</span>
		</div>
		{#if hover}
			<div class="readout num" data-testid="mac-bars-readout" style:transform="translate({hover.sx + 12}px, {hover.sy - 30}px)">
				{view.rows.label} {view.rows.ticks[hover.row]} · {view.columns.label} {view.columns.ticks[hover.column]} ·
				{view.value_label} {cellText(value(hover.row, hover.column))}
			</div>
		{/if}
		{#if failure}
			<div class="failure" role="alert">{failure}</div>
		{/if}
	</div>
	<div class="legend" aria-label="Color legend">
		<div class="ltitle">{view.value_label}</div>
		<div class="bar" style:background={colormapGradient()}></div>
		<div class="ticks num">
			{#each legendTicks as t, i (i)}<span>{t}</span>{/each}
		</div>
		<span class="count">{formatNumber(ROWS * COLUMNS, 6)} bars</span>
	</div>
</section>

<style>
	.bars {
		display: flex;
		flex-direction: column;
		min-height: 0;
		height: 100%;
		gap: var(--sp-2);
	}
	.title {
		margin: 0;
		font-size: var(--fs-2);
		font-weight: 600;
	}
	.stage {
		position: relative;
		flex: 1 1 auto;
		min-height: 260px;
		overflow: hidden;
		border-radius: var(--radius);
		background: var(--bg-2);
	}
	.canvas {
		position: absolute;
		inset: 0;
		cursor: grab;
	}
	.canvas:active {
		cursor: grabbing;
	}
	.canvas :global(canvas) {
		display: block;
		width: 100% !important;
		height: 100% !important;
	}
	.overlay {
		position: absolute;
		inset: 0;
		pointer-events: none;
		overflow: hidden;
	}
	.tick {
		position: absolute;
		left: 0;
		top: 0;
		font-size: 10.5px;
		color: var(--text-2);
		white-space: nowrap;
		visibility: hidden;
	}
	.tick.row,
	.tick.col {
		margin: -7px 0 0 -14px;
	}
	.tick.name {
		color: var(--text-3);
		font-size: 11px;
		margin: -7px 0 0 -14px;
	}
	.readout {
		position: absolute;
		left: 0;
		top: 0;
		padding: 2px 6px;
		font-size: var(--fs-0);
		color: var(--text-1);
		background: var(--bg-1);
		border: 1px solid var(--line-1);
		border-radius: var(--radius);
		pointer-events: none;
		white-space: nowrap;
	}
	.failure {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		color: var(--text-2);
	}
	.legend {
		display: flex;
		align-items: center;
		gap: var(--sp-2);
		font-size: var(--fs-0);
		color: var(--text-2);
	}
	.legend .bar {
		width: 120px;
		height: 8px;
		border-radius: 2px;
	}
	.legend .ticks {
		display: flex;
		gap: var(--sp-2);
	}
	.legend .count {
		margin-left: auto;
		color: var(--text-3);
	}
</style>
