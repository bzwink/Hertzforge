<script lang="ts" module>
	/** How each result's stage was left: a drag or a choice in it replaces the view, and the view belongs to the person, so the new one opens as they left it. */
	const poses = new Map<string, { position: number[]; target: number[]; up: number[]; facing: 'perspective' | 'top' | 'front'; logX: boolean; logZ: boolean }>();
</script>

<script lang="ts">
	import { copyCanvas, plotBackground } from './copyImage';
	/**
	 * Waterfall view, drawn with three.js: every record a polyline at its own depth, colored by
	 * level on the sequential colormap (viridis), the way the 3-D data view draws time
	 * histories, FRFs and PSDs.
	 *
	 * All records are one geometry and one draw call, whatever their number. The raw values go to
	 * the GPU once as attributes (abscissa, level, record); the vertex shader maps them onto the
	 * stage and takes log10 of a log axis, and the fragment shader looks the level up in the
	 * colormap, so orbiting and switching an axis between linear and log move no data. Only the
	 * segment index is rebuilt on a scale change, because a level at or below zero is a gap on a
	 * log axis. Frames are drawn on demand: a still scene costs nothing.
	 *
	 * Hovering reads the point under the cursor by drawing the curves once more, into a few pixels
	 * around it, with each vertex writing its own index as its color; one small draw however many
	 * points there are, where a search on the CPU would visit every segment.
	 *
	 * The same pane draws the stages: records standing at named stations (two
	 * densities paired at each channel, a specification's limits beside its target), drawn in
	 * their role's color where they are not the measurement, with translucent limit bands, the
	 * lines past an abort limit as rectangles, and a scalogram as a surface inside the walls of
	 * its cone of influence. `regions` stand across the whole stage on the floor and the back wall,
	 * as a spectrum's do behind its curves, and one that drags is moved the way it is on the plot:
	 * its middle slides it and an edge moves that edge.
	 */
	import { onDestroy, onMount, untrack } from 'svelte';
	import {
		BufferAttribute,
		BufferGeometry,
		Color,
		DoubleSide,
		DataTexture,
		GLSL3,
		LinearFilter,
		LineBasicMaterial,
		LineSegments,
		Mesh,
		MeshBasicMaterial,
		NoBlending,
		PerspectiveCamera,
		RawShaderMaterial,
		RGBAFormat,
		Scene,
		Vector3,
		WebGLRenderer,
		WebGLRenderTarget,
		type Material
	} from 'three';
	import { TrackballControls } from './trackball';
	import type { ParamValues } from '$lib/forms/params';
	import type { Role, WaterfallView } from '$lib/protocol/types';
	import { cssVar, theme } from '$lib/theme/theme.svelte';
	import { colormap, colormapGradient } from './colormap';
	import { formatCount, formatNumber, formatTick, withUnit } from './format';
	import { ROLE_STYLES } from './roles';
	import {
		STAGE,
		axisTicks,
		exceedTriangles,
		fraction,
		levelRange,
		namedRecords,
		pickedPoint,
		recordOf,
		segmentIndex,
		stageMap,
		onStage,
		regionMarks,
		stationOf,
		surfaceIndex,
		unfraction,
		wallTriangles,
		zoneTriangles
	} from './waterfall';

	interface Props {
		view: WaterfallView;
		/** Run the view's operation again with these params: a quantity chosen here. */
		onparams?: (patch: ParamValues) => void;
		/** A draggable region released at new edges: its index in `view.regions`, and the edges. */
		onspan?: (region: number, x0: number, x1: number) => void;
		/** The result the view belongs to: its stage reopens as it was left within it. */
		scope?: string;
	}

	let { view, onparams, onspan, scope }: Props = $props();
	// read once: the pane is made again for each view it shows
	const saved = untrack(() => (scope ? poses.get(scope) : undefined));

	/** Side of the square read back around the cursor, in pixels: lines are one pixel wide. */
	const PICK = 9;

	/** The roles drawn in their own color, in the order of the shader's `uTones`; 'measured' is 0, by level. */
	const TONES: Role[] = ['reference', 'tolerance', 'abort', 'limit', 'drive', 'other'];

	/** The stage's marks, by what they mean: their color variable and how much they show through. */
	const MARKS = {
		warning: { colorVar: '--series-tolerance', opacity: 0.12, label: 'Between warning and abort' },
		over: { colorVar: '--series-abort', opacity: 0.1, label: 'Past abort, above' },
		under: { colorVar: '--blue-2', opacity: 0.1, label: 'Past abort, below' },
		overHit: { colorVar: '--series-abort', opacity: 0.6, label: 'Lines above the abort limit' },
		underHit: { colorVar: '--blue-2', opacity: 0.6, label: 'Lines below the abort limit' },
		wall: { colorVar: '--text-3', opacity: 0.14, label: 'Cone of influence' }
	} as const;
	type MarkKind = keyof typeof MARKS;

	type Facing = 'perspective' | 'top' | 'front';

	let canvasHost: HTMLDivElement;
	let overlay: HTMLDivElement;

	let logX = $state(false);
	let logZ = $state(false);
	let facing = $state<Facing>('perspective');
	let failure = $state<string | null>(null);
	/** The point under the cursor and where it stands on screen, or null. */
	let hover = $state<{ point: number; record: number; sx: number; sy: number } | null>(null);

	// The service says how each axis reads; the viewer may change it afterward.
	$effect.pre(() => {
		logX = saved?.logX ?? view.x.scale === 'log';
		logZ = saved?.logZ ?? view.z.scale === 'log';
	});

	const recordCount = $derived(view.records.length);
	/** A stage of paths: every point at a depth of its own on the `y` axis (the sine stage's time). */
	const pathed = $derived(!!view.y_values && !!view.y_range);
	const yRange = $derived.by((): [number, number] => {
		const [a, b] = view.y_range ?? [0, 1];
		return a < b ? [a, b] : [a, a + 1];
	});
	const yTicks = $derived(pathed ? axisTicks(yRange, false).filter((t) => t >= yRange[0] && t <= yRange[1]) : []);
	/** A point's depth on the stage: along its path, or at its record's station. */
	function depthAt(point: number, record: number): number {
		if (pathed && view.y_values) return Math.min(1, Math.max(0, (view.y_values[point] - yRange[0]) / (yRange[1] - yRange[0]))) * STAGE.y;
		const station = view.depths ? view.depths[record] : record;
		return stationCount > 1 ? (station / (stationCount - 1)) * STAGE.y : 0;
	}
	/** The depth axis: the stations when the view names them, else one a record. */
	const stationCount = $derived(view.stations?.length ?? view.records.length);
	const depthLabels = $derived(view.stations ?? view.records);
	const pointCount = $derived(view.x_values.length);
	const xRange = $derived.by((): [number, number] => {
		const [a, b] = view.x_range;
		if (!logX) return a < b ? [a, b] : [a, a + 1];
		// a log abscissa starts at the first positive value drawn
		let lo = Infinity;
		const x = view.x_values;
		for (let i = 0; i < x.length; i++) if (x[i] > 0 && x[i] < lo) lo = x[i];
		const top = b > 0 ? b : 1;
		return [Math.min(Number.isFinite(lo) ? lo : top / 10, top / 1.0001), top];
	});
	const zRange = $derived(levelRange(view.z_values, logZ, view.z_range));
	const xTicks = $derived(axisTicks(xRange, logX).filter((t) => t >= xRange[0] && t <= xRange[1]));
	const zTicks = $derived(axisTicks(zRange, logZ).filter((t) => t >= zRange[0] * (1 - 1e-9) && t <= zRange[1] * (1 + 1e-9)));
	const named = $derived(namedRecords(stationCount));
	/** The marks this view has, for the key beside the legend. */
	const markKinds = $derived.by((): MarkKind[] => {
		const kinds = new Set<MarkKind>();
		for (const z of view.zones ?? []) kinds.add(z.kind === 'warning' ? 'warning' : z.side === 'upper' ? 'over' : 'under');
		for (const e of view.exceed ?? []) kinds.add(e.side === 'upper' ? 'overHit' : 'underHit');
		if (view.walls?.length) kinds.add('wall');
		return [...kinds];
	});
	/** The roles drawn in a color of their own, for the key. */
	const toneRoles = $derived(TONES.filter((r) => view.roles?.includes(r)));

	// three.js objects (not reactive)
	let renderer: WebGLRenderer | null = null;
	let scene: Scene;
	let camera: PerspectiveCamera;
	let controls: TrackballControls;
	let geometry: BufferGeometry;
	let curves: LineSegments | Mesh;
	const marks = new Map<MarkKind, Mesh>();
	/** The regions' shading and edges, rebuilt on a scale change or as one is dragged. */
	let regionFaces: Mesh | null = null;
	let regionEdges: LineSegments | null = null;
	/** A region being dragged, with its edges as they stand. */
	let draggingRegion: { k: number; x0: number; x1: number } | null = null;
	let frame: LineSegments | null = null;
	let colorTexture: DataTexture;
	const materials: Material[] = [];
	const uniforms = {
		uX: { value: [0, 1] },
		uZ: { value: [0, 1] },
		uXLog: { value: 0 },
		uZLog: { value: 0 },
		uDepth: { value: 0 },
		uPath: { value: 0 },
		uMap: { value: null as DataTexture | null },
		uTones: { value: TONES.map(() => new Color()) }
	};
	let raf = 0;
	let dirty = true;
	let curveMaterial: RawShaderMaterial;
	let pickMaterial: RawShaderMaterial;
	let pickTarget: WebGLRenderTarget;
	const pickPixels = new Uint8Array(4 * PICK * PICK);
	/** A cursor position waiting to be picked at the next frame, in canvas pixels. */
	let pendingPick: [number, number] | null = null;
	let records = new Float32Array(0);
	let resizeObs: ResizeObserver | null = null;
	const centre = new Vector3(STAGE.x / 2, STAGE.y / 2, STAGE.z / 2);

	const vertexShader = /* glsl */ `
		#ifdef PICKING
		flat out uint vId;
		#endif
		in float ax;
		in float az;
		in float ay;
		in float arec;
		in float atone;
		flat out float vTone;
		uniform mat4 modelViewMatrix;
		uniform mat4 projectionMatrix;
		uniform vec2 uX;
		uniform vec2 uZ;
		uniform float uXLog;
		uniform float uZLog;
		uniform float uDepth;
		uniform float uPath;
		out float vLevel;
		const vec3 S = vec3(${STAGE.x}, ${STAGE.y}, ${STAGE.z});
		void main() {
			float x = uXLog > 0.5 ? log2(ax) * 0.30102999566 : ax;
			float z = uZLog > 0.5 ? log2(az) * 0.30102999566 : az;
			float tx = (x - uX.x) / (uX.y - uX.x);
			float tz = clamp((z - uZ.x) / (uZ.y - uZ.x), 0.0, 1.0);
			float depth = uPath > 0.5 ? ay : (uDepth > 0.0 ? arec / uDepth : 0.0);
			vLevel = tz;
			vTone = atone;
			#ifdef PICKING
			vId = uint(gl_VertexID) + 1u;
			#endif
			gl_Position = projectionMatrix * modelViewMatrix * vec4(tx * S.x, depth * S.y, tz * S.z, 1.0);
		}`;

	// The colormap's stops are sRGB and the canvas is sRGB, so the looked-up color is written as is.
	const fragmentShader = /* glsl */ `
		precision highp float;
		uniform sampler2D uMap;
		uniform vec3 uTones[${TONES.length}];
		in float vLevel;
		flat in float vTone;
		out vec4 outColor;
		void main() {
			int tone = int(vTone + 0.5);
			outColor = vec4(tone > 0 ? uTones[tone - 1] : texture(uMap, vec2(vLevel, 0.5)).rgb, 1.0);
		}`;

	// a vertex's index, plus one so that zero is "nothing here", in four bytes
	const pickFragmentShader = /* glsl */ `
		precision highp float;
		flat in uint vId;
		flat in float vTone;
		out vec4 outColor;
		void main() {
			outColor = vec4(uvec4(vId, vId >> 8u, vId >> 16u, vId >> 24u) & 255u) / 255.0;
		}`;

	function mapTexture(): DataTexture {
		const n = 256;
		const data = new Uint8Array(4 * n);
		const rgb: [number, number, number] = [0, 0, 0];
		for (let i = 0; i < n; i++) {
			colormap(i / (n - 1), rgb);
			data[4 * i] = Math.round(rgb[0] * 255);
			data[4 * i + 1] = Math.round(rgb[1] * 255);
			data[4 * i + 2] = Math.round(rgb[2] * 255);
			data[4 * i + 3] = 255;
		}
		const texture = new DataTexture(data, n, 1, RGBAFormat);
		texture.magFilter = LinearFilter;
		texture.minFilter = LinearFilter;
		texture.needsUpdate = true;
		return texture;
	}

	function span(range: [number, number], log: boolean): [number, number] {
		return log ? [Math.log10(range[0]), Math.log10(range[1])] : range;
	}

	function applyScales() {
		if (!renderer) return;
		uniforms.uX.value = span(xRange, logX);
		uniforms.uZ.value = span(zRange, logZ);
		uniforms.uXLog.value = logX ? 1 : 0;
		uniforms.uZLog.value = logZ ? 1 : 0;
		const index = view.surface ? surfaceIndex : segmentIndex;
		geometry.setIndex(new BufferAttribute(index(view.offsets, view.x_values, view.z_values, logX, logZ), 1));
		buildFrame();
		buildMarks();
		buildRegions();
		dirty = true;
	}

	/** The floor, the back wall and the left wall, with a grid line at every tick. */
	function buildFrame() {
		if (frame) {
			scene.remove(frame);
			frame.geometry.dispose();
		}
		const p: number[] = [];
		const seg = (a: number[], b: number[]) => p.push(...a, ...b);
		const { x: X, y: Y, z: Z } = STAGE;
		seg([0, 0, 0], [X, 0, 0]);
		seg([X, 0, 0], [X, Y, 0]);
		seg([X, Y, 0], [0, Y, 0]);
		seg([0, Y, 0], [0, 0, 0]);
		seg([0, Y, 0], [0, Y, Z]);
		seg([X, Y, 0], [X, Y, Z]);
		seg([0, Y, Z], [X, Y, Z]);
		for (const t of xTicks) {
			const f = fraction(t, xRange, logX) * X;
			seg([f, 0, 0], [f, Y, 0]);
			seg([f, Y, 0], [f, Y, Z]);
		}
		for (const t of zTicks) {
			const f = fraction(t, zRange, logZ) * Z;
			seg([0, Y, f], [X, Y, f]);
			seg([X, Y, f], [X + 0.012, Y, f]);
		}
		const g = new BufferGeometry();
		g.setAttribute('position', new BufferAttribute(new Float32Array(p), 3));
		frame = new LineSegments(g, frameMaterial);
		frame.renderOrder = -1;
		scene.add(frame);
	}

	let frameMaterial: LineBasicMaterial;

	/** Each region on the floor and the back wall, the one being dragged at its edges as they stand. */
	function buildRegions() {
		for (const old of [regionFaces, regionEdges]) {
			if (!old) continue;
			scene.remove(old);
			old.geometry.dispose();
			(old.material as Material).dispose();
		}
		regionFaces = regionEdges = null;
		const regions = view.regions ?? [];
		if (!regions.length) return;
		const faces: number[] = [];
		const edges: number[] = [];
		regions.forEach((r, k) => {
			const [x0, x1] = draggingRegion?.k === k ? [draggingRegion.x0, draggingRegion.x1] : [r.x0, r.x1];
			const m = regionMarks(fraction(x0, xRange, logX) * STAGE.x, fraction(x1, xRange, logX) * STAGE.x);
			faces.push(...m.faces);
			edges.push(...m.edges);
		});
		const f = new BufferGeometry();
		f.setAttribute('position', new BufferAttribute(new Float32Array(faces), 3));
		regionFaces = new Mesh(f, new MeshBasicMaterial({ color: new Color(cssVar('--blue-2')), transparent: true, opacity: 0.16, side: DoubleSide, depthWrite: false }));
		regionFaces.renderOrder = 1;
		const e = new BufferGeometry();
		e.setAttribute('position', new BufferAttribute(new Float32Array(edges), 3));
		regionEdges = new LineSegments(e, new LineBasicMaterial({ color: new Color(cssVar('--blue-2')), transparent: true, opacity: 0.9, depthTest: false }));
		regionEdges.renderOrder = 2;
		scene.add(regionFaces, regionEdges);
		dirty = true;
	}

	/** Where the pointer meets the stage: the abscissa there, and the depth it met at. */
	function pointerOnStage(e: PointerEvent): { x: number; depth: number } | null {
		const box = canvasHost.getBoundingClientRect();
		const ndc = new Vector3(((e.clientX - box.left) / box.width) * 2 - 1, -((e.clientY - box.top) / box.height) * 2 + 1, 0.5).unproject(camera);
		const dir = ndc.sub(camera.position).normalize();
		const hit = onStage([camera.position.x, camera.position.y, camera.position.z], [dir.x, dir.y, dir.z]);
		if (!hit) return null;
		return { x: unfraction(hit[0] / STAGE.x, xRange, logX), depth: hit[1] };
	}

	/** How far stage x `a` lies from the pointer on screen, at the depth the pointer met the stage. */
	function pixelsFrom(x: number, depth: number, e: PointerEvent): number {
		const box = canvasHost.getBoundingClientRect();
		probe.set(fraction(x, xRange, logX) * STAGE.x, depth, 0).project(camera);
		return Math.hypot(((probe.x + 1) / 2) * box.width - (e.clientX - box.left), ((1 - probe.y) / 2) * box.height - (e.clientY - box.top));
	}

	/**
	 * A press on a draggable region takes it from the camera: the middle slides it, an edge (when
	 * the span has two) moves that edge, and on release its new edges go to `onspan`.
	 */
	function onRegionPress(e: PointerEvent) {
		if (!renderer || !onspan || e.button !== 0 || !view.regions?.length) return;
		const at = pointerOnStage(e);
		if (!at) return;
		const k = view.regions.findIndex(
			(r) => r.drag && ((at.x >= Math.min(r.x0, r.x1) && at.x <= Math.max(r.x0, r.x1)) || pixelsFrom(r.x0, at.depth, e) <= EDGE_PX || pixelsFrom(r.x1, at.depth, e) <= EDGE_PX)
		);
		if (k < 0) return;
		const r = view.regions[k];
		const two = !!r.drag!.high;
		const part = two && pixelsFrom(r.x0, at.depth, e) <= EDGE_PX ? 'low' : two && pixelsFrom(r.x1, at.depth, e) <= EDGE_PX ? 'high' : 'move';
		e.preventDefault();
		e.stopPropagation();
		controls.enabled = false;
		hover = null;
		const start = at.x;
		draggingRegion = { k, x0: r.x0, x1: r.x1 };
		const move = (m: PointerEvent) => {
			const now = pointerOnStage(m);
			if (!now || !draggingRegion) return;
			if (part === 'move') {
				const d = now.x - start;
				draggingRegion = { k, x0: r.x0 + d, x1: r.x1 + d };
			} else if (part === 'low') draggingRegion = { k, x0: Math.min(now.x, r.x1), x1: r.x1 };
			else draggingRegion = { k, x0: r.x0, x1: Math.max(now.x, r.x0) };
			buildRegions();
		};
		const up = () => {
			window.removeEventListener('pointermove', move);
			window.removeEventListener('pointerup', up);
			controls.enabled = true;
			const done = draggingRegion;
			draggingRegion = null;
			if (!done || (done.x0 === r.x0 && done.x1 === r.x1)) return buildRegions();
			onspan?.(k, done.x0, done.x1);
		};
		window.addEventListener('pointermove', move);
		window.addEventListener('pointerup', up);
	}

	/** How near a region's edge, in pixels, a press takes that edge rather than the whole span. */
	const EDGE_PX = 8;

	/** The bands, the exceedances and the walls, one translucent mesh for each kind, rebuilt on a scale change. */
	function buildMarks() {
		const map = stageMap(xRange, logX, zRange, logZ, stationCount, pathed ? yRange : undefined);
		const found = new Map<MarkKind, number[]>();
		const into = (kind: MarkKind) => found.get(kind) ?? (found.set(kind, []), found.get(kind)!);
		for (const z of view.zones ?? []) zoneTriangles(z, map, into(z.kind === 'warning' ? 'warning' : z.side === 'upper' ? 'over' : 'under'));
		for (const e of view.exceed ?? []) exceedTriangles(e, map, into(e.side === 'upper' ? 'overHit' : 'underHit'));
		for (const w of view.walls ?? []) wallTriangles(w, map, into('wall'));
		for (const [kind, mesh] of marks) {
			scene.remove(mesh);
			mesh.geometry.dispose();
			(mesh.material as Material).dispose();
			marks.delete(kind);
		}
		for (const [kind, positions] of found) {
			if (!positions.length) continue;
			const g = new BufferGeometry();
			g.setAttribute('position', new BufferAttribute(new Float32Array(positions), 3));
			const material = new MeshBasicMaterial({
				color: new Color(cssVar(MARKS[kind].colorVar)),
				transparent: true,
				opacity: MARKS[kind].opacity,
				side: DoubleSide,
				depthWrite: false
			});
			const mesh = new Mesh(g, material);
			mesh.renderOrder = 1;
			scene.add(mesh);
			marks.set(kind, mesh);
		}
	}

	function applyTheme() {
		if (!renderer) return;
		frameMaterial.color = new Color(cssVar('--line-2'));
		TONES.forEach((role, k) => uniforms.uTones.value[k].set(cssVar(ROLE_STYLES[role].colorVar)));
		for (const [kind, mesh] of marks) (mesh.material as MeshBasicMaterial).color.set(cssVar(MARKS[kind].colorVar));
		for (const r of [regionFaces, regionEdges]) if (r) (r.material as MeshBasicMaterial | LineBasicMaterial).color.set(cssVar('--blue-2'));
		dirty = true;
	}

	/** The distance at which the whole stage fits the pane, in whichever direction is tighter. */
	function fitDistance(): number {
		const radius = 0.5 * Math.hypot(STAGE.x, STAGE.y, STAGE.z);
		const vertical = (camera.fov * Math.PI) / 360;
		const horizontal = Math.atan(Math.tan(vertical) * camera.aspect);
		return (radius / Math.sin(Math.min(vertical, horizontal))) * 1.12;
	}

	function face(which: Facing) {
		facing = which;
		if (!renderer) return;
		const d = fitDistance();
		controls.target.copy(centre);
		if (which === 'top') {
			camera.position.set(centre.x, centre.y - 0.0001, centre.z + d);
			camera.up.set(0, 1, 0);
		} else if (which === 'front') {
			camera.position.set(centre.x, centre.y - d, centre.z);
			camera.up.set(0, 0, 1);
		} else {
			camera.position.copy(centre).addScaledVector(new Vector3(0.62, -1.25, 0.72).normalize(), d);
			camera.up.set(0, 0, 1);
		}
		controls.update();
		dirty = true;
	}

	function init() {
		try {
			// the drawing is kept after each frame so a printed report can capture the canvas
			renderer = new WebGLRenderer({ antialias: true, alpha: true, powerPreference: 'high-performance', preserveDrawingBuffer: true });
		} catch {
			failure = 'The waterfall needs WebGL, which is not available in this browser.';
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
		camera = new PerspectiveCamera(30, 1, 0.01, 100);

		geometry = new BufferGeometry();
		geometry.setAttribute('ax', new BufferAttribute(view.x_values, 1));
		geometry.setAttribute('az', new BufferAttribute(view.z_values, 1));
		records = recordOf(view.offsets, view.x_values.length);
		geometry.setAttribute('arec', new BufferAttribute(stationOf(view.offsets, view.x_values.length, view.depths), 1));
		// on a stage of paths each point carries its own depth, as a fraction of the y span
		const along = new Float32Array(view.x_values.length);
		if (pathed && view.y_values) {
			const span = yRange[1] - yRange[0];
			for (let i = 0; i < along.length; i++) along[i] = Math.min(1, Math.max(0, (view.y_values[i] - yRange[0]) / span));
		}
		geometry.setAttribute('ay', new BufferAttribute(along, 1));
		uniforms.uPath.value = pathed ? 1 : 0;
		const tones = new Float32Array(view.x_values.length);
		if (view.roles) for (let i = 0; i < tones.length; i++) tones[i] = TONES.indexOf(view.roles[records[i]]) + 1;
		geometry.setAttribute('atone', new BufferAttribute(tones, 1));
		colorTexture = mapTexture();
		uniforms.uMap.value = colorTexture;
		uniforms.uDepth.value = Math.max(0, stationCount - 1);
		curveMaterial = new RawShaderMaterial({ glslVersion: GLSL3, vertexShader, fragmentShader, uniforms, side: DoubleSide });
		materials.push(curveMaterial);
		curves = view.surface ? new Mesh(geometry, curveMaterial) : new LineSegments(geometry, curveMaterial);
		curves.frustumCulled = false;
		scene.add(curves);
		pickMaterial = new RawShaderMaterial({
			glslVersion: GLSL3,
			vertexShader,
			fragmentShader: pickFragmentShader,
			uniforms,
			defines: { PICKING: 1 },
			blending: NoBlending,
			side: DoubleSide
		});
		materials.push(pickMaterial);
		pickTarget = new WebGLRenderTarget(PICK, PICK);

		frameMaterial = new LineBasicMaterial({ transparent: true, opacity: 0.8 });
		materials.push(frameMaterial);

		controls = new TrackballControls(camera, renderer.domElement);
		controls.zoomSpeed = 0.9;
		controls.addEventListener('change', () => (dirty = true));

		applyTheme();
		applyScales();

		resizeObs = new ResizeObserver(resize);
		resizeObs.observe(canvasHost);
		resize();
		face(saved?.facing ?? view.facing ?? facing);
		if (saved) {
			camera.position.fromArray(saved.position);
			camera.up.fromArray(saved.up);
			controls.target.fromArray(saved.target);
			controls.update();
		}
		raf = requestAnimationFrame(loop);
	}

	function resize() {
		if (!renderer) return;
		const w = Math.max(1, canvasHost.clientWidth);
		const h = Math.max(1, canvasHost.clientHeight);
		renderer.setSize(w, h, false);
		renderer.domElement.style.width = '100%';
		renderer.domElement.style.height = '100%';
		camera.aspect = w / h;
		camera.updateProjectionMatrix();
		dirty = true;
	}

	const probe = new Vector3();
	/** Closest two record labels may stand on screen, in pixels; nearer ones are skipped. */
	const LABEL_GAP_PX = 12;

	/**
	 * Pin each HTML label over its point on the stage. Record and level labels are thinned by where
	 * they land on screen, not only by count: forty names along a depth axis seen end-on, or a
	 * decade every few pixels on a short level axis, would overprint.
	 */
	function placeLabels() {
		if (!overlay) return;
		const w = canvasHost.clientWidth;
		const h = canvasHost.clientHeight;
		const last: Record<string, [number, number]> = {};
		for (const el of overlay.querySelectorAll<HTMLElement>('[data-at]')) {
			const [x, y, z] = el.dataset.at!.split(',').map(Number);
			probe.set(x, y, z).project(camera);
			const sx = ((probe.x + 1) / 2) * w;
			const sy = ((1 - probe.y) / 2) * h;
			let visible = probe.z < 1;
			const thinned = el.classList.contains('rec') ? 'rec' : el.classList.contains('z') ? 'z' : null;
			if (visible && thinned) {
				const [lx, ly] = last[thinned] ?? [-Infinity, -Infinity];
				if (Math.hypot(sx - lx, sy - ly) < LABEL_GAP_PX) visible = false;
				else last[thinned] = [sx, sy];
			}
			el.style.visibility = visible ? 'visible' : 'hidden';
			el.style.transform = `translate(${sx}px, ${sy}px)`;
		}
	}

	/** Draw the curves alone around (px, py) with the picking material and read back what is there. */
	function pick(px: number, py: number) {
		if (!renderer) return;
		const w = canvasHost.clientWidth;
		const h = canvasHost.clientHeight;
		const half = (PICK - 1) / 2;
		camera.setViewOffset(w, h, px - half, py - half, PICK, PICK);
		curves.material = pickMaterial;
		if (frame) frame.visible = false;
		for (const mesh of marks.values()) mesh.visible = false;
		renderer.setRenderTarget(pickTarget);
		renderer.clear();
		renderer.render(scene, camera);
		renderer.setRenderTarget(null);
		renderer.readRenderTargetPixels(pickTarget, 0, 0, PICK, PICK, pickPixels);
		curves.material = curveMaterial;
		if (frame) frame.visible = true;
		for (const mesh of marks.values()) mesh.visible = true;
		camera.clearViewOffset();
		const point = pickedPoint(pickPixels, PICK);
		if (point === null || point >= records.length) {
			hover = null;
			return;
		}
		const record = records[point];
		probe
			.set(
				fraction(view.x_values[point], xRange, logX) * STAGE.x,
				depthAt(point, record),
				Math.min(1, Math.max(0, fraction(view.z_values[point], zRange, logZ))) * STAGE.z
			)
			.project(camera);
		hover = { point, record, sx: ((probe.x + 1) / 2) * w, sy: ((1 - probe.y) / 2) * h };
	}

	function onPointerMove(e: PointerEvent) {
		// not while orbiting, when the point under the cursor changes every frame anyway
		if (e.buttons) {
			hover = null;
			return;
		}
		const box = canvasHost.getBoundingClientRect();
		pendingPick = [e.clientX - box.left, e.clientY - box.top];
	}

	function loop() {
		raf = requestAnimationFrame(loop);
		if (!renderer) return;
		const moved = controls.update();
		if (moved && hover) hover = null;
		if (pendingPick && !moved) {
			pick(...pendingPick);
			pendingPick = null;
		}
		if (!dirty && !moved) return;
		dirty = false;
		renderer.render(scene, camera);
		placeLabels();
		// kept as it is drawn, not as the pane goes: the view that replaces it is made first
		if (scope) poses.set(scope, { position: camera.position.toArray(), target: controls.target.toArray(), up: camera.up.toArray(), facing, logX, logZ });
	}

	onMount(init);

	$effect(() => {
		void logX;
		void logZ;
		void xRange;
		void zRange;
		applyScales();
	});

	$effect(() => {
		void theme.version;
		applyTheme();
	});

	// Labels are rendered by Svelte; place them once the DOM has them.
	$effect(() => {
		void xTicks;
		void zTicks;
		void named;
		queueMicrotask(() => {
			placeLabels();
		});
	});

	onDestroy(() => {
		cancelAnimationFrame(raf);
		resizeObs?.disconnect();
		if (!renderer) return;
		controls.dispose();
		geometry.dispose();
		frame?.geometry.dispose();
		for (const mesh of [...marks.values(), regionFaces, regionEdges]) {
			if (!mesh) continue;
			mesh.geometry.dispose();
			(mesh.material as Material).dispose();
		}
		colorTexture.dispose();
		pickTarget.dispose();
		for (const m of materials) m.dispose();
		renderer.dispose();
		renderer.forceContextLoss();
		renderer.domElement.remove();
		renderer = null;
	});

	function onKey(e: KeyboardEvent) {
		if (e.target instanceof HTMLInputElement || e.metaKey || e.ctrlKey || e.altKey) return;
		const k = e.key.toLowerCase();
		if (k === 'r') face('perspective');
		else if (k === 't') face('top');
		else if (k === 'f') face('front');
		else if (k === 'x') logX = !logX;
		else if (k === 'l') logZ = !logZ;
		else return;
		e.preventDefault();
	}

	function tickText(v: number): string {
		return formatTick(v);
	}

	const legendTicks = $derived.by(() => {
		const [lo, hi] = zRange;
		const mid = logZ ? Math.sqrt(lo * hi) : (lo + hi) / 2;
		return [lo, mid, hi].map((v) => formatNumber(v, 3));
	});

	const xAt = (t: number) => `${fraction(t, xRange, logX) * STAGE.x},${-0.035},0`;
	// the level axis stands at the back right, where a spectrum's levels are usually lowest, so
	// the curves rarely pass in front of its labels
	const zAt = (t: number) => `${STAGE.x + 0.02},${STAGE.y},${fraction(t, zRange, logZ) * STAGE.z}`;
	// the records are named at the low end of the abscissa, as the earlier toolset named them
	// (Brandon, 2026-11-14)
	const recordAt = (r: number) => `${-0.03},${stationCount > 1 ? (r / (stationCount - 1)) * STAGE.y : 0},0`;
	const yAt = (t: number) => `${-0.03},${((t - yRange[0]) / (yRange[1] - yRange[0])) * STAGE.y},0`;
	/** A path is named where it starts, a little above its first point. */
	const pathStart = (r: number) => {
		const p = view.offsets[r];
		if (p >= view.offsets[r + 1]) return null;
		const z = Math.min(1, Math.max(0, fraction(view.z_values[p], zRange, logZ))) * STAGE.z;
		return `${fraction(view.x_values[p], xRange, logX) * STAGE.x},${depthAt(p, r) - 0.03 * STAGE.y},${z + 0.04 * STAGE.z}`;
	};

	let copiedImage = $state<'copied' | 'saved' | null>(null);
	/** The view as it stands, as an image on the clipboard: drawn and copied in one task. */
	async function copyImage() {
		const drawer = renderer;
		if (!drawer) return;
		copiedImage = await copyCanvas(drawer.domElement, plotBackground(drawer.domElement), () => drawer.render(scene, camera), view.title);
		if (copiedImage) setTimeout(() => (copiedImage = null), 1500);
	}
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
	class="waterfall"
	data-testid="view-waterfall"
	tabindex="0"
	role="group"
	aria-label="{view.title}. {recordCount} records. R resets the view, T looks from the top, F from the front, X and L switch the abscissa and the level between linear and log."
	onkeydown={onKey}
>
	<!-- svelte-ignore a11y_no_static_element_interactions (hovering only reads; the keyboard has the view's own keys) -->
	<div
		class="canvas"
		data-testid="waterfall-canvas"
		bind:this={canvasHost}
		onpointerdowncapture={onRegionPress}
		onpointermove={onPointerMove}
		onpointerleave={() => ((pendingPick = null), (hover = null))}
	></div>

	{#if hover}
		<div class="marker" style:transform="translate({hover.sx}px, {hover.sy}px)" aria-hidden="true"></div>
		<div class="readout num" data-testid="waterfall-readout" style:transform="translate({hover.sx + 12}px, {hover.sy - 30}px)">
			<span class="rname">{view.records[hover.record]}</span>
			<span>{formatNumber(view.x_values[hover.point], 5)}{view.x.unit ? ` ${view.x.unit}` : ''}</span>
			<span>{formatNumber(view.z_values[hover.point], 4)}{view.z.unit ? ` ${view.z.unit}` : ''}</span>
			{#if pathed && view.y && view.y_values}<span>{formatNumber(view.y_values[hover.point], 4)}{view.y.unit ? ` ${view.y.unit}` : ''}</span>{/if}
		</div>
	{/if}

	<div class="overlay" bind:this={overlay} aria-hidden="true">
		{#each xTicks as t (t)}
			<span class="tick x num" data-at={xAt(t)}>{tickText(t)}</span>
		{/each}
		{#if facing !== 'top'}
			<!-- seen from above, every level lands on one point -->
			{#each zTicks as t (t)}
				<span class="tick z num" data-at={zAt(t)}>{tickText(t)}</span>
			{/each}
		{/if}
		{#if pathed}
			{#each yTicks as t (t)}
				<span class="tick rec low num" data-at={yAt(t)}>{tickText(t)}</span>
			{/each}
			{#each view.records as label, r (r)}
				{@const at = pathStart(r)}
				{#if at}<span class="tick rec" data-at={at}>{label}</span>{/if}
			{/each}
			{#if view.y}<span class="tick rec low" data-at="{-0.03},{STAGE.y * 1.08},0">{withUnit(view.y.label, view.y.unit)}</span>{/if}
		{:else}
			{#each named as r (r)}
				<span class="tick rec low" data-at={recordAt(r)}>{depthLabels[r]}</span>
			{/each}
		{/if}
		{#each view.regions ?? [] as r, i (i)}
			{#if r.label}<span class="tick region" data-at="{((fraction(r.x0, xRange, logX) + fraction(r.x1, xRange, logX)) / 2) * STAGE.x},{STAGE.y},{STAGE.z * 1.04}">{r.label}</span>{/if}
		{/each}
		<span class="title" data-at="{STAGE.x / 2},-0.11,0">{withUnit(view.x.label, view.x.unit)}</span>
	</div>

	{#if failure}
		<div class="failure" role="alert">{failure}</div>
	{/if}

	<div class="top">
		<div class="toolbar" role="toolbar" aria-label="Waterfall display">
			<button class="btn ghost sm copyimg" data-testid="waterfall-copy-image" onclick={copyImage} title="Copy the 3-D view as an image">{copiedImage === 'copied' ? 'Copied' : copiedImage === 'saved' ? 'Saved as PNG' : 'Copy Image'}</button>
			<div class="seg" role="radiogroup" aria-label="Viewpoint">
				<button role="radio" data-testid="waterfall-perspective" aria-checked={facing === 'perspective'} class:on={facing === 'perspective'} onclick={() => face('perspective')} title="Perspective (R)">3-D</button>
				<button role="radio" data-testid="waterfall-top" aria-checked={facing === 'top'} class:on={facing === 'top'} onclick={() => face('top')} title="From the top: a map of level over abscissa and record (T)">Top</button>
				<button role="radio" data-testid="waterfall-front" aria-checked={facing === 'front'} class:on={facing === 'front'} onclick={() => face('front')} title="From the front: every record on one axis (F)">Front</button>
			</div>
			<div class="seg" role="radiogroup" aria-label="{view.x.label} scale">
				<span class="lbl">{view.x.label}</span>
				<button role="radio" data-testid="waterfall-x-linear" aria-checked={!logX} class:on={!logX} onclick={() => (logX = false)} title="Linear (X)">Lin</button>
				<button role="radio" data-testid="waterfall-x-log" aria-checked={logX} class:on={logX} onclick={() => (logX = true)} title="Log (X)">Log</button>
			</div>
			<div class="seg" role="radiogroup" aria-label="{view.z.label} scale">
				<span class="lbl">{view.z.label}</span>
				<button role="radio" data-testid="waterfall-z-linear" aria-checked={!logZ} class:on={!logZ} onclick={() => (logZ = false)} title="Linear (L)">Lin</button>
				<button role="radio" data-testid="waterfall-z-log" aria-checked={logZ} class:on={logZ} onclick={() => (logZ = true)} title="Log (L)">Log</button>
			</div>
			{#if view.quantities && onparams}
				<div class="seg" role="radiogroup" aria-label="Quantity">
					<span class="lbl">Quantity</span>
					{#each view.quantities as q (q.value)}
						<button
							role="radio"
							data-testid="waterfall-quantity"
							aria-checked={q.value === view.quantity}
							class:on={q.value === view.quantity}
							onclick={() => q.value !== view.quantity && onparams({ quantity: q.value, page: 1 })}
							title="{formatCount(q.records)} records">{q.label}</button
						>
					{/each}
				</div>
			{/if}
		</div>
		{#if view.note}<div class="note">{view.note}</div>{/if}
	</div>

	<div class="bottom">
		<div class="legend" aria-label="Color legend">
			<div class="ltitle">{withUnit(view.z.label, view.z.unit)}{logZ ? ', log' : ''}</div>
			<div class="bar" style:background={colormapGradient()}></div>
			<div class="ticks num">
				{#each legendTicks as t, i (i)}<span>{t}</span>{/each}
			</div>
		</div>
		{#if markKinds.length || toneRoles.length}
			<ul class="key" aria-label="Key" data-testid="waterfall-key">
				{#each toneRoles as role (role)}
					<li><span class="line" style:background="var({ROLE_STYLES[role].colorVar})"></span>{role === 'reference' ? 'Reference' : role === 'tolerance' ? 'Warning limits' : role === 'abort' ? 'Abort limits' : role}</li>
				{/each}
				{#each markKinds as kind (kind)}
					<li><span class="swatch" style:background="var({MARKS[kind].colorVar})" style:opacity={Math.max(0.45, MARKS[kind].opacity)}></span>{MARKS[kind].label}</li>
				{/each}
			</ul>
		{/if}
		<div class="stats num" aria-live="off">
			{#if view.page}<span data-testid="waterfall-page">Page {view.page.number} of {view.page.count}, {formatCount(view.page.records)} records</span>{/if}
			{#if view.stations && !view.surface}<span>{formatCount(stationCount)} stations</span>{/if}
			<span>{formatCount(recordCount)} records</span>
			<span>{formatCount(pointCount)} points</span>
		</div>
	</div>
</div>

<style>
	.copyimg {
		flex: none;
		font-size: var(--text-2xs, 11px);
	}

	.waterfall {
		position: relative;
		height: 100%;
		min-height: 320px;
		background:
			radial-gradient(ellipse at 50% 35%, var(--bg-3), transparent 70%),
			var(--bg-plot);
		border: 1px solid var(--line-1);
		border-radius: var(--radius-md);
		overflow: hidden;
	}

	.waterfall:focus-visible {
		outline-offset: -2px;
	}

	.canvas,
	.overlay {
		position: absolute;
		inset: 0;
	}

	.canvas :global(canvas) {
		display: block;
		touch-action: none;
	}

	.overlay {
		pointer-events: none;
		overflow: hidden;
	}

	.tick,
	.title {
		position: absolute;
		left: 0;
		top: 0;
		white-space: nowrap;
		font-size: var(--text-2xs);
		color: var(--text-2);
		text-shadow:
			0 0 3px var(--bg-plot),
			0 0 2px var(--bg-plot);
		will-change: transform;
	}

	.tick.x {
		translate: -50% 2px;
	}

	.tick.z {
		translate: 4px -50%;
	}

	.tick.region {
		translate: -50% -100%;
		color: var(--blue-1);
	}

	.tick.rec {
		translate: 4px -50%;
		color: var(--text-3);
	}

	/* at the low end of the abscissa a label ends at its point, clear of the stage */
	.tick.rec.low {
		translate: calc(-100% - 4px) -50%;
	}

	.title {
		translate: -50% 0;
		color: var(--text-1);
		font-size: var(--text-xs);
	}

	.top,
	.bottom {
		position: absolute;
		left: var(--sp-2);
		right: var(--sp-3);
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: var(--sp-2) var(--sp-4);
		pointer-events: none;
	}

	.top {
		top: var(--sp-2);
		align-items: flex-start;
	}

	.bottom {
		bottom: var(--sp-2);
		left: var(--sp-3);
		align-items: flex-end;
	}

	.toolbar {
		pointer-events: auto;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--sp-2);
		padding: 4px;
		background: color-mix(in srgb, var(--bg-1) 88%, transparent);
		border: 1px solid var(--line-1);
		border-radius: var(--radius-md);
		backdrop-filter: blur(6px);
	}

	.seg {
		display: inline-flex;
		align-items: center;
		padding: 2px;
		background: var(--bg-2);
		border: 1px solid var(--line-2);
		border-radius: var(--radius-md);
	}

	.seg .lbl {
		padding: 0 var(--sp-2) 0 var(--sp-1);
		font-size: var(--text-xs);
		color: var(--text-3);
	}

	.seg button {
		height: 20px;
		padding: 0 var(--sp-2);
		font-size: var(--text-xs);
		color: var(--text-2);
		background: transparent;
		border: 0;
		border-radius: 4px;
	}

	.seg button.on {
		color: var(--text-1);
		background: var(--bg-4);
		box-shadow: inset 0 0 0 1px var(--line-3);
	}

	.note {
		padding-top: 6px;
		font-size: var(--text-xs);
		color: var(--text-3);
	}

	.legend {
		width: 200px;
		padding-bottom: 2px;
	}

	.ltitle {
		margin-bottom: 4px;
		font-size: var(--text-2xs);
		color: var(--text-2);
	}

	.bar {
		height: 9px;
		border-radius: 2px;
		box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.25);
	}

	.ticks {
		display: flex;
		justify-content: space-between;
		margin-top: 3px;
		font-size: var(--text-2xs);
		color: var(--text-2);
	}

	.key {
		display: flex;
		flex-wrap: wrap;
		gap: 2px var(--sp-3);
		margin: 0;
		padding: 0;
		list-style: none;
		font-size: var(--text-2xs);
		color: var(--text-2);
	}

	.key li {
		display: inline-flex;
		align-items: center;
		gap: 5px;
	}

	.key .line {
		width: 14px;
		height: 2px;
	}

	.key .swatch {
		width: 10px;
		height: 10px;
		border-radius: 2px;
	}

	.stats {
		display: flex;
		gap: var(--sp-3);
		font-size: var(--text-2xs);
		color: var(--text-3);
	}

	.marker {
		position: absolute;
		top: -4px;
		left: -4px;
		width: 8px;
		height: 8px;
		border: 1.5px solid var(--text-1);
		border-radius: 50%;
		pointer-events: none;
	}

	.readout {
		position: absolute;
		top: 0;
		left: 0;
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding: 4px 6px;
		font-size: var(--text-2xs);
		color: var(--text-1);
		white-space: nowrap;
		background: color-mix(in srgb, var(--bg-1) 90%, transparent);
		border: 1px solid var(--line-2);
		border-radius: 4px;
		pointer-events: none;
	}

	.rname {
		color: var(--text-2);
	}

	.failure {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		padding: var(--sp-6);
		color: var(--text-2);
		text-align: center;
	}
</style>
