/** Shaka 5.2.12 plain-wvtt display adapter.
 * The pinned HTML renderer omits the WebVTT percentage line:center branch.
 * Keep all parsing, selection, overlap and lifecycle behavior in Shaka; add the
 * missing axis-specific alignment after its normal caption layout. Deliberately
 * fail on another build: this small override uses a versioned internal hook.
 */
export function createWvttDisplayer(player, shaka) {
  if (shaka.Player.version !== 'v5.2.12-debug' ||
      typeof shaka.text.UITextDisplayer.prototype.setCaptionStyles_ !== 'function') {
    throw new Error('wvtt display adapter requires Shaka 5.2.12 compiled.debug');
  }
  class WvttDisplayer extends shaka.text.UITextDisplayer {
    setCaptionStyles_(element, cue, parents, hasWrapper) {
      super.setCaptionStyles_(element, cue, parents, hasWrapper);
      if (cue.line === null || cue.lineAlign !== 'center' ||
          cue.lineInterpretation !== shaka.text.Cue.lineInterpretation.PERCENTAGE) return;
      const style = element.style;
      style.position = 'absolute';
      let shift;
      if (cue.writingMode !== 'horizontal-tb') {
        // The cross-axis size is the line width, not the video width. Shaka's
        // inline-axis centering transform also needs to follow vertical writing.
        style.width = 'max-content';
        style.transform = style.transform.replace('translateX(-50%)', 'translateY(-50%)');
      }
      if (cue.writingMode === 'vertical-lr') {
        style.left = cue.line+'%'; style.right = ''; shift = 'translateX(-50%)';
      } else if (cue.writingMode === 'vertical-rl') {
        style.right = cue.line+'%'; style.left = ''; shift = 'translateX(50%)';
      } else {
        style.top = cue.line+'%'; style.bottom = ''; shift = 'translateY(-50%)';
      }
      // Preserve the independent inline-axis position alignment from Shaka.
      style.transform = [style.transform, shift].filter(Boolean).join(' ');
    }
  }
  return new WvttDisplayer(player);
}
