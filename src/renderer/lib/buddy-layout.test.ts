import { describe, it, expect } from 'vitest';
import { getChatPanelPlacement } from './buddy-layout';

const viewport = { width: 1200, height: 800 };

describe('getChatPanelPlacement', () => {
  it('anchors below the buddy when there is room', () => {
    const placement = getChatPanelPlacement({ x: 500, y: 400 }, viewport);
    // buddy height 100 + margin 8 = 508 below buddy top
    expect(placement.top).toBe(400 + 100 + 8);
    expect(placement.width).toBe(280);
  });

  it('flips above the buddy when there is no room below', () => {
    const placement = getChatPanelPlacement({ x: 500, y: 760 }, viewport);
    // 760 + 100 + 40 + 8 = 908 > 800, so flip above: top = 760 - 40 - 8
    expect(placement.top).toBe(760 - 40 - 8);
  });

  it('clamps left edge at the viewport margin', () => {
    const placement = getChatPanelPlacement({ x: -50, y: 400 }, viewport);
    expect(placement.left).toBe(8);
  });

  it('clamps right edge at viewport width minus panel width and margin', () => {
    const placement = getChatPanelPlacement({ x: 1300, y: 400 }, viewport);
    expect(placement.left).toBe(viewport.width - 280 - 8);
  });

  it('centers the panel under the buddy when there is room on both sides', () => {
    const placement = getChatPanelPlacement({ x: 500, y: 400 }, viewport);
    // buddy center = 500 + 80; panel left = (500 + 80) - 140 = 440
    expect(placement.left).toBe(440);
  });

  it('clamps top to the margin when buddy is near the top and flipping above', () => {
    // Force flip-above by being too close to bottom
    const tinyViewport = { width: 1200, height: 110 };
    const placement = getChatPanelPlacement({ x: 500, y: 5 }, tinyViewport);
    // Would otherwise be 5 - 40 - 8 = -43, should clamp to 8
    expect(placement.top).toBe(8);
  });
});
