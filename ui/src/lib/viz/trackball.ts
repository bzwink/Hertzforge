/**
 * Turning a 3D view as a trackball camera does (VTK's, which the earlier toolset used): a drag
 * across turns the camera about the view's up direction, a drag up or down turns it about the
 * screen's horizontal, and the up direction turns with it, so the view tumbles over the top with
 * no stop. Panning and zooming are OrbitControls': right drag (or a left drag with Shift, Ctrl or
 * Cmd) pans, middle drag and the wheel zoom.
 *
 * OrbitControls is not used because it orbits about the up direction it reads once, when it is
 * made: every pane made it with the camera's default Y up and then set Z up, so a drag across
 * spun a waterfall about its records rather than its level, and stopped at the Y poles
 * (Brandon, 2026-11-14).
 */
import { EventDispatcher, PerspectiveCamera, Quaternion, Vector3 } from 'three';

/** Degrees a drag across the whole pane turns the view: VTK's 20 times its motion factor of 10. */
export const TURN_PER_PANE = 200;

type Mode = 'rotate' | 'pan' | 'dolly' | null;

const _offset = new Vector3();
const _axis = new Vector3();
const _right = new Vector3();
const _up = new Vector3();
const _turn = new Quaternion();

/** The camera turned about `target` by `across` and `down` degrees, as the drag reads them. */
export function tumble(camera: PerspectiveCamera, target: Vector3, across: number, down: number): void {
	_offset.copy(camera.position).sub(target);
	// a drag to the right turns the scene right: the camera goes left about the up direction
	_axis.copy(camera.up).normalize();
	_turn.setFromAxisAngle(_axis, (-across * Math.PI) / 180);
	_offset.applyQuaternion(_turn);
	// a drag down tips the scene toward the viewer: the camera rises, the up direction with it
	_right.crossVectors(_offset, camera.up).normalize();
	if (_right.lengthSq() > 0) {
		_turn.setFromAxisAngle(_right, (down * Math.PI) / 180);
		_offset.applyQuaternion(_turn);
		_up.copy(camera.up).applyQuaternion(_turn);
	} else _up.copy(camera.up);
	// the up direction kept square to the line of sight, as VTK's OrthogonalizeViewUp does
	const sight = _offset.clone().normalize();
	_up.addScaledVector(sight, -_up.dot(sight)).normalize();
	camera.position.copy(target).add(_offset);
	camera.up.copy(_up);
	camera.lookAt(target);
}

export class TrackballControls extends EventDispatcher<{ change: object }> {
	readonly camera: PerspectiveCamera;
	readonly domElement: HTMLElement;
	target = new Vector3();
	enabled = true;
	zoomSpeed = 1;
	panSpeed = 1;
	private mode: Mode = null;
	private pointer = -1;
	private last = { x: 0, y: 0 };
	private moved = false;
	private readonly listeners: [string, EventListener, AddEventListenerOptions?][];

	constructor(camera: PerspectiveCamera, domElement: HTMLElement) {
		super();
		this.camera = camera;
		this.domElement = domElement;
		this.listeners = [
			['pointerdown', this.onDown as EventListener],
			['pointermove', this.onMove as EventListener],
			['pointerup', this.onUp as EventListener],
			['pointercancel', this.onUp as EventListener],
			['wheel', this.onWheel as EventListener, { passive: false }],
			['contextmenu', ((e: Event) => this.enabled && e.preventDefault()) as EventListener]
		];
		for (const [name, fn, options] of this.listeners) domElement.addEventListener(name, fn, options);
		domElement.style.touchAction = 'none';
	}

	/** Whether the camera moved since the last call; the camera looks at the target. */
	update(): boolean {
		this.camera.lookAt(this.target);
		const moved = this.moved;
		this.moved = false;
		return moved;
	}

	dispose(): void {
		for (const [name, fn, options] of this.listeners) this.domElement.removeEventListener(name, fn, options);
	}

	private changed(): void {
		this.moved = true;
		this.dispatchEvent({ type: 'change' });
	}

	private onDown = (e: PointerEvent): void => {
		if (!this.enabled || this.mode) return;
		const modified = e.ctrlKey || e.metaKey || e.shiftKey;
		this.mode = e.button === 0 ? (modified ? 'pan' : 'rotate') : e.button === 1 ? 'dolly' : e.button === 2 ? 'pan' : null;
		if (!this.mode) return;
		this.pointer = e.pointerId;
		this.last = { x: e.clientX, y: e.clientY };
		this.domElement.setPointerCapture(e.pointerId);
	};

	private onMove = (e: PointerEvent): void => {
		if (!this.mode || e.pointerId !== this.pointer) return;
		const dx = e.clientX - this.last.x;
		const dy = e.clientY - this.last.y;
		this.last = { x: e.clientX, y: e.clientY };
		if (!this.enabled || (!dx && !dy)) return;
		const { clientWidth: width, clientHeight: height } = this.domElement;
		if (this.mode === 'rotate') {
			tumble(this.camera, this.target, (dx / Math.max(width, 1)) * TURN_PER_PANE, (dy / Math.max(height, 1)) * TURN_PER_PANE);
		} else if (this.mode === 'pan') this.pan(dx * this.panSpeed, dy * this.panSpeed, height);
		else if (dy) this.dolly(dy > 0 ? 1 / this.zoomScale(dy) : this.zoomScale(dy));
		this.changed();
	};

	private onUp = (e: PointerEvent): void => {
		if (e.pointerId !== this.pointer) return;
		if (this.domElement.hasPointerCapture(e.pointerId)) this.domElement.releasePointerCapture(e.pointerId);
		this.mode = null;
		this.pointer = -1;
	};

	private onWheel = (e: WheelEvent): void => {
		if (!this.enabled || this.mode) return;
		e.preventDefault();
		// as OrbitControls reads a wheel: lines and pages in pixels, a pinch ten times a scroll
		let delta = e.deltaY * (e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 100 : 1);
		if (e.ctrlKey) delta *= 10;
		if (!delta) return;
		this.dolly(delta > 0 ? 1 / this.zoomScale(delta) : this.zoomScale(delta));
		this.changed();
	};

	private zoomScale(delta: number): number {
		return Math.pow(0.95, this.zoomSpeed * Math.abs(delta * 0.01));
	}

	/** The camera moved toward the target, its distance times `scale`. */
	private dolly(scale: number): void {
		_offset.copy(this.camera.position).sub(this.target).multiplyScalar(scale);
		this.camera.position.copy(this.target).add(_offset);
	}

	/** Camera and target slid together, by as much as the pixels dragged cover at the target. */
	private pan(dx: number, dy: number, height: number): void {
		const camera = this.camera;
		const reach = camera.position.distanceTo(this.target) * Math.tan((camera.fov * Math.PI) / 360);
		const scale = (2 * reach) / Math.max(height, 1);
		camera.updateMatrix();
		_right.setFromMatrixColumn(camera.matrix, 0).multiplyScalar(-dx * scale);
		_up.setFromMatrixColumn(camera.matrix, 1).multiplyScalar(dy * scale);
		_offset.copy(_right).add(_up);
		camera.position.add(_offset);
		this.target.add(_offset);
	}
}
