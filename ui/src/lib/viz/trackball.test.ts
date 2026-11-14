import { describe, expect, it } from 'vitest';
import { PerspectiveCamera, Vector3 } from 'three';
import { tumble } from './trackball';

/** A camera in front of the origin, looking along +Y with Z up: a waterfall's level is Z. */
function front(): PerspectiveCamera {
	const camera = new PerspectiveCamera(30, 1, 0.01, 100);
	camera.position.set(0, -10, 0);
	camera.up.set(0, 0, 1);
	camera.lookAt(0, 0, 0);
	return camera;
}

describe('tumble', () => {
	it('turns about the up direction on a drag across, as the earlier toolset did', () => {
		// the waterfall spun about its records, Y, rather than its level (Brandon, 2026-11-14)
		const camera = front();
		tumble(camera, new Vector3(), 90, 0);
		expect(camera.position.z).toBeCloseTo(0);
		expect(camera.position.length()).toBeCloseTo(10);
		expect(camera.position.x).toBeCloseTo(-10, 5);
		expect(camera.up.z).toBeCloseTo(1);
	});

	it('turns about the screen horizontal on a drag down, the camera rising', () => {
		const camera = front();
		tumble(camera, new Vector3(), 0, 30);
		expect(camera.position.z).toBeGreaterThan(0);
		expect(camera.position.length()).toBeCloseTo(10);
	});

	it('goes over the top with no stop, the up direction turning with it', () => {
		const camera = front();
		const target = new Vector3();
		for (let k = 0; k < 18; k++) tumble(camera, target, 0, 10);
		// 180 degrees on: behind the target and upside down
		expect(camera.position.y).toBeCloseTo(10, 4);
		expect(camera.up.z).toBeCloseTo(-1, 4);
		expect(camera.position.distanceTo(target)).toBeCloseTo(10);
	});

	it('keeps the up direction square to the line of sight', () => {
		const camera = front();
		const target = new Vector3(1, 2, 3);
		for (let k = 0; k < 40; k++) tumble(camera, target, 7, -11);
		const sight = target.clone().sub(camera.position).normalize();
		expect(Math.abs(camera.up.dot(sight))).toBeLessThan(1e-9);
		expect(camera.up.length()).toBeCloseTo(1);
	});
});
