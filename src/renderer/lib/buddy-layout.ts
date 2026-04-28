interface ChatPanelPlacement {
  left: number;
  top: number;
  width: number;
}

const PANEL_WIDTH = 280;
const PANEL_HEIGHT = 40;
const MARGIN = 8;
const BUDDY_WIDTH = 160;
const BUDDY_HEIGHT = 100;

// Anchors a chat panel to the buddy's position. Flips above the buddy when there's
// not enough room below, and clamps horizontally so the panel never overflows the viewport.
export function getChatPanelPlacement(
  buddy: { x: number; y: number },
  viewport: { width: number; height: number },
): ChatPanelPlacement {
  const anchorBelow = buddy.y + BUDDY_HEIGHT + PANEL_HEIGHT + MARGIN < viewport.height;
  const left = Math.max(
    MARGIN,
    Math.min(viewport.width - PANEL_WIDTH - MARGIN, buddy.x + BUDDY_WIDTH / 2 - PANEL_WIDTH / 2),
  );
  const top = anchorBelow
    ? buddy.y + BUDDY_HEIGHT + MARGIN
    : Math.max(MARGIN, buddy.y - PANEL_HEIGHT - MARGIN);
  return { left, top, width: PANEL_WIDTH };
}
