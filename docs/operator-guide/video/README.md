# Rehearse a projected image and fade

Use a spare display and a short video with a visible subject. This exercise checks the route, stacking and fade that an audience would see.

1. Open **Preferences → Outputs**. Assign the display to the intended output. A QLab import creates extra stage outputs as floating windows so you can choose the Linux screen by sight.
2. Select that output’s alignment tools and show the grid. Align the grid on the projection surface. Drag the centre to move the picture, and drag a corner to pin it. For fine control, Tab to a handle and press the arrow keys. Shift increases the step from one thousandth to one hundredth of the output dimension. Clear the pattern before running the cue.
3. Add a Video cue with the test clip and choose the calibrated output. Give it a cue number. Put it on layer 500 and set its base opacity to zero. Give the clip enough duration to cover the fade.
4. Add a Fade cue immediately afterwards and select the video as its target. Enable the brightness fade, set its target to 75% and duration to five seconds. Leave **Stop at end** off.
5. GO the Video cue. Check that the correct output stays dark. GO the Fade cue and watch the image rise. Press Stop, return the playhead to the Video cue and repeat. If a show uses stacked content, repeat with a second clip on a higher layer and check the overlap.

## Reconnect a live camera on Linux

1. Connect a USB camera or a V4L2 capture adapter and aim it at a labelled rehearsal card. Add a Camera cue and assign a spare output in **Layer**. In **Camera**, select **Device**, rescan and choose the camera marked **by port** when available. Save the workspace.
2. Start the cue and look for the card on the rehearsal display. Stop the cue and unplug the camera. Rescan and open **Check Workspace**. The Camera cue keeps its saved port and reports the missing input. An unplugged input also leaves the camera off if it disappears during the cue’s Pre-Wait.
3. Reconnect it to the same port and rescan. Check the workspace again and give the cue a GO. The same saved route should show the card even if Linux assigned a different `/dev/video` number. Try another USB port between runs, then explicitly choose that new connection if it becomes the show route.
4. On a migrated or older show, verify the actual feed before replacing a saved numbered device with its port option. If the driver only exposes a numbered device, confirm the image again after reconnecting or changing the hardware. Repeat the check after opening the show on the venue computer.

A port assignment identifies the computer connection, including the hub path. Confirm the physical camera at that connection. Check Workspace verifies that local input paths resolve to V4L2 nodes. Use the rehearsal display to check permissions, the selected capture stream, framing and latency. A Network URL is saved as authored and needs its own end-to-end rehearsal. [QLab Camera cues and input checks](https://qlab.app/docs/v5/video/camera-cues/)

## Move a picture across the stage

1. On a spare output, place a rehearsal still on an Image cue. A small high-contrast subject makes it easy to see the edge against black. In **Geometry**, use a scale of `0.5`, set X to `-0.35`, and leave Y at `0`. Start the cue and check that the subject is left of centre.
2. Add a Fade that targets the Image individually. Leave **Crossfade** set to **None**, turn **Fade brightness** and **Fade volume** off, and give the Fade an eight-second duration. In **Position, scale and rotation**, enable **Move horizontally** and enter `0.35`. Leave **Move vertically** clear. Use a straight curve for the first rehearsal.
3. GO the Fade. Pause the Fade halfway across the screen and check that the subject holds. Resume and check that the subject reaches the right-hand position on the same horizontal line. Stop the Fade during another run to leave the subject at its current position. The next position Fade starts from that held position. A new GO of the Image uses its saved Geometry.
4. Rehearse a manual Geometry correction during the move. The correction takes control from the Fade. Then save and reopen the show to check the destination and run it again. The position change applies to the target’s current playback, including a still on a separate output.

The destination uses the Geometry inspector’s units. `0` centres the picture on an axis, and `0.35` offsets it by 35% of its scaled width or height. A QLab migration needs a visual comparison against the original stage because QLab specifies translation in pixels. Crop fades need separate review. [QLab video geometry fades](https://qlab.app/docs/v5/video/fading-video/)

### Route the picture through a waypoint

1. Start the rehearsal Image at its saved left-hand position again. On the eight-second Fade, enable **Move vertically** and set **Destination Y** to `0`. Keep **Destination X** at `0.35`.
2. Choose **Add waypoint**. Enter X `0` and Y `-0.45` for the waypoint. Give the Fade a GO and look for the top-centre location at four seconds, then the right-hand destination at eight seconds.
3. Add a second waypoint to pass two stage marks. Use **Earlier** and **Later** to change the visiting order. The inspector shows the new percentage for each waypoint. Rehearse Pause and Resume, then save and reopen the workspace to confirm the route.
4. Return to the one-waypoint setup. Stop and restart the Image, turn on **Smooth path**, then give the Fade a GO. Watch the curved approach to the top-centre mark and compare the picture at two, four and eight seconds. It passes the same four-second mark and settles at the same destination. Turn **Smooth path** off to return to straight legs on the next GO.

The path starts from the target’s live position when the Fade begins. It uses up to sixteen waypoints with equal time on each leg. Its X and Y coordinates travel together. Smooth paths use a shared direction and speed where two legs meet. Two identical consecutive waypoints hold the picture for that leg. Curve settings continue to shape the Fade’s other enabled parameters. Remove the waypoints to return to a direct move with separate rising and falling curves. Changes to waypoints or smoothing take effect on the next GO. Rebuild important imported paths by comparing the endpoints and intermediate marks with the original stage. [QLab Fading With Paths](https://qlab.app/docs/v5/tutorials/fading-with-paths/)

### Repeat the same picture path

1. In the one-waypoint rehearsal, set the Fade to four seconds. Keep **Do Not Continue** selected, turn off **Stop at end**, and leave position as the only parameter enabled. In **Waypoints**, turn on **Loop path**. It works with straight legs or **Smooth path**.
2. Start the Image at its saved left-hand position, then give the Fade a GO. Every four seconds the picture moves from the destination back to its original left-hand position and starts the next pass. To avoid that jump, set the X and Y destinations to the position where the Image begins.
3. Pause the Fade partway through a pass, wait several seconds, and resume it. Watch the path and the progress bar continue from the paused point. Stop the Fade mid-pass. The picture stays at its last position while the Image keeps playing.
4. Start the Fade again, then change the Image’s Geometry. The manual edit takes control. Try restarting the Image during another run and check that the new playback uses its saved Geometry.

Each GO takes a new starting position from the live target. That position stays fixed across the passes of that GO. The Fade duration is one pass, and each pass visits the same waypoints. Looping continues until the Fade is stopped or loses control of its target. [QLab video geometry path loops](https://qlab.app/docs/v5/video/fading-video/#2d-paths)

## Grow or shrink a live picture

1. Place a rehearsal still with a border on an Image cue. Use **Fit**, with a centred position and **Scale** at 30%. Start the Image and mark the visible left and right edges.
2. Add a four-second Fade targeting the Image. In **Position, scale and rotation**, enable **Fade scale** and enter 70 in **Destination scale (%)**. Leave brightness and position clear if you want to check size in isolation.
3. GO the Fade and pause it halfway through. The picture holds its current size. Resume and measure the final width. For a linear curve the width is about 50% of the fitted frame at two seconds and 70% at the end.
4. GO a second Fade to 40% to rehearse the move from the live 70% size. Correct the target’s Geometry during the move to take manual control. Stop the picture and give it a fresh GO to check that it returns to the saved 30%.

Use the same control with a Video, Image or Camera target. Scale follows the rising curve when growing and the falling curve when shrinking. To combine resizing and movement, enable both on the same Fade. Position offsets are fractions of the current picture size, so increasing scale increases the physical offset of a picture away from centre. For a migration, build the Fade in Inkue and compare its projected size and timing with the reference desk. Inkue uses one uniform scale from 5% to 800% of the fitted frame. QLab provides separate X and Y scaling and uses source pixels in its custom geometry. [QLab video geometry fades](https://qlab.app/docs/v5/video/fading-video/)

## Repeat a move from the current picture

1. Start a still on a spare output with X at `−0.50`, Y at `0` and scale at 80%. Add a Fade with the still as its only target, leave **Crossfade** at **None**, and turn off **Fade brightness**, **Fade volume** and **Stop at end**.
2. In **Position, scale and rotation**, choose **Relative** under **Position and scale values**. Enable **Move horizontally** and enter `0.25` for **Offset X**. Enable **Fade scale** and set **Scale multiplier** to 50%. Keep vertical movement off and use a two-second duration.
3. Run the Fade twice on the same picture. After the first run, its X setting has moved to `−0.25` and it is 40% of fitted size. After the second run, it is centred and 20% of fitted size. Stop the Image and give it a fresh GO to see the saved position and size.
4. Save and reopen the workspace. Confirm **Relative**, then repeat with Pre-Wait. The Fade takes its starting position and scale when Pre-Wait ends. A Geometry edit during the Fade takes over from it.

Relative X and Y offsets add to the live values. The scale multiplier multiplies the live scale. Endpoints stay within ±1 of picture width or height and 5–800% scale. Rotation, saturation and brightness still have their own absolute destinations. Choose **Absolute** to use position waypoints. Switching the value basis keeps the numbers displayed, so check their purpose before running the Fade. Rebuild relative moves from QLab in the corresponding Inkue units and compare them on the intended stage. [QLab relative video fades](https://qlab.app/docs/v5/video/fading-video/#relative-fades)

## Preview a picture's saturation

1. Start an Image on a rehearsal output and leave a second colored picture visible on a different layer.
2. Add a Fade targeting the first Image, enable **Fade saturation**, and set its destination to **0%**. Select **Preview saturation**. The first Image appears in grayscale for about two seconds. Select **End saturation preview** to dismiss it sooner.
3. Preview again and run the Fade before the sample expires. The Fade takes over from the picture's live saturation. Once it finishes, **Revert Picture Fade** can restore that value. Check that the second layer still has its own color.

Preview uses a running, revealed Video, Image, or Camera cue on a visible output. Stage a picture with Load to check the eligibility rule, then start it before previewing. The sample uses the Fade's saved destination and leaves cue timing, saved settings, and picture levels other than saturation alone. [QLab Live Fade Preview](https://qlab.app/docs/v5/tools/tools-menu/#live-fade-preview)

## Revert a completed picture Fade

1. Start a colourful rehearsal Image on a spare output. Add a two-second Fade that moves it to the right and fades its saturation to 0%. Turn off **Fade brightness**, **Fade volume** and **Stop at end**.
2. GO the Fade. Once it finishes, select that Fade and choose **Revert Picture Fade** in its **Live picture** section. Check that the still returns to its starting position and colour while it is running.
3. Run the Fade again. After it finishes, edit the Image’s **Geometry**. Use **Revert Picture Fade** again and check that its colour returns at the new position. You can also run another saturation Fade first to give that Fade control of the colour.
4. Repeat the original Fade, stop its Image and give the Image a fresh GO. The new playback uses its saved settings. The revert command tells you when the original playback has ended.

Revert applies once to the latest completed Fade on the same armed Video, Image or Camera playback. Position, scale and still-image rotation share Geometry ownership. Each color control can be taken over and restored independently. A Reset cue targeting the Fade clears its revert record. [QLab Reverting Fades](https://qlab.app/docs/v5/video/fading-video/#reverting-fades)

## Mask the edge of a projector

1. In **Preferences → Display**, select the output you intend to align. Use **White** to put a field on its screen while the stage is available for calibration.
2. Under **Output mask and feather**, enter **10** in **Left → Trim %** and press **Enter**. The left tenth of the picture goes black. Enter **5** in **Left → Feather %**. The next five percent now ramps from black to full brightness. The preview shows these settings before the corner pin. The projector shows them with the pin applied.
3. To tune a manual overlap, first feed the matching overlap picture to both output routes. Set the right feather of the left projector and the left feather of the right projector to the measured overlap percentage for each picture. Check a white field, grey field and show material from the audience position. The ramps are linear in pixel value. Projector black level and gamma affect the physical sum.
4. Clear the white pattern, save the workspace, reopen it and confirm each output’s own mask. **Clear edges** removes trim and feather from the selected output. **Reset alignment** keeps its mask.

For an irregular surface, create a still PNG matching the output aspect ratio. For example, paint a white arch on black, with a short grey ramp along the top. Under **Image mask**, choose **Load PNG…**. Check the white test pattern: the area outside the arch should be black, the body bright, and the ramp dimmer. The mask stretches to the picture and moves with its corner pins. Trim and feather multiply with it. Transparent pixels block light. **Remove image mask** keeps the edge settings.

Save and reopen the show after moving the source PNG. Each output’s normalized grayscale mask travels inside the show, including its collected and recovery copies. For a migrated QLab file, load the stage mask PNG for each corresponding Inkue output. Import accepts still PNG files up to 4096 pixels per side and 32 MiB. [QLab stage masks and regions](https://qlab.app/docs/v5/video/video-output/)

## Rearrange two pictures on one projector

1. Open **Preferences → Display** for the main output or select an extra output in **Preferences → Outputs**. Confirm its screen assignment. Use **SMPTE Bars** under **Test Patterns** for an image whose left and right sides are visibly different.
2. In **Stage regions**, add two regions. Leave **Top** at 0% and **Height** at 100% in all four rectangles. Set **Width** to 50% in all four rectangles.
3. In the first region, set **Left** to 50% under **Source frame** and 0% under **Projector raster**. In the second, set them to 0% and 50% respectively. The two diagrams show which side each region reads and where it will appear.
4. Choose **Apply region layout**. Check that the bars have exchanged halves. Give one destination a narrower width to confirm that the uncovered area goes black. Restore its width, apply the layout and clear the test pattern.
5. Route a Video cue to the output and GO it. Check the swapped picture, its audio output meters, and the result of Hard Stop. Save and reopen to verify the layout. To return to the original picture, remove both regions and apply the empty layout.

Use up to eight rectangular regions per output. Repeating a source rectangle in two places samples the same frame twice and keeps a single video and audio playback path. Later regions cover earlier ones in an overlap. The whole projector’s corner pins, trim, feather and PNG mask apply to the arranged picture, including its black gaps. Test patterns, text and the on-output timer are part of the picture being arranged.

For a QLab single-route stage, write down each region’s source and destination rectangle before moving the show. The dimensions in Inkue are percentages, with zero at the top-left of each raster. To convert a QLab region whose lower-left is measured from the center of a stage, add half the stage width to the lower-left X for **Left**. Subtract the lower-left Y and the region height from half the stage height for **Top**. Divide each result and size by the relevant stage dimension and multiply by 100. Enter destination coordinates against their own raster. Compare the saved layout and one real cue from the original desk. [QLab video regions](https://qlab.app/docs/v5/video/video-output/#understanding-regions) · [QLab regional stages workshop](https://qlab.app/cookbook/regional-stages/)

## Align a still across two projectors

1. In a rehearsal copy, add the second projector in **Preferences → Outputs**. Assign the two outputs to separate screens. Use floating windows for desk rehearsal. Stop visual cues before changing the layout.
2. Choose **Set up paired projection…**, select **Side by side**, and assign the left and right outputs. For edge-to-edge projection, leave **Overlap of full picture** at 0%. For beams that overlap, enter the overlap as a percentage of the entire image. Choose **Apply paired crops**. Each selected output receives one source crop mapped to its whole raster. Its corner pins, mask, and feather settings remain available for fine adjustment.
3. Choose **Save alignment PNG…**. Add a simultaneous Group with two **Image** children referencing that PNG. In each Image cue’s **Geometry** settings choose **Stretch**. In **Compositing**, route one child to each selected output. GO the Group and align the white diagonals and dashed amber overlap guides on the surface. For equal 16:9 beams the chart’s shape matches the combined picture. With 10% shared overlap, each beam contains about 18% shared image along its inside edge. Tune those edges with the projector’s trim and feather controls at the venue.
4. Stop the chart Group and substitute the still for the show on both Image cues. Save and reopen, then confirm both crops and Hard Stop in rehearsal. The two Image cues are silent. Use **Collect and Save** to carry their PNG with the show.

The paired crop is stored in each output’s region settings and applies to every picture on that output. Clear each output’s **Stage regions** to remove the split. Moving images still use independent playback contexts. Shared movie timing, blended layouts of three or more projectors and per-region mesh warping remain open. [QLab multi-projector regions](https://qlab.app/docs/v5/video/video-output/#understanding-regions)

### Check a feathered seam at the booth

1. Start with the two silent alignment Image cues. Match both projectors’ color mode, input range, brightness and transfer gamma. Using the chart at the intended throw and screen size, physically overlap the beams until the diagonals and dashed guides coincide. Use individual corner pins to fix geometry.
2. With the Image Group stopped, open **Set up paired projection…**. Enter the measured positive overlap as a percentage of the whole picture. Enable **Feather shared seam** and enter the matched display setting under **Projector gamma**. The **2.20** starting value suits projectors configured for a power-law gamma of about 2.2. Apply both crops together.
3. Restart the chart. Sweep a light meter along each gray and white band from one beam through the overlap into the other. Check the black band from audience positions as well. Each ramp reduces a different fraction of light at the same source coordinate. The projected result depends on beam alignment, brightness, transfer response and black floor. Existing trim, edge feather and image masks still reduce light, so check for a second dimming ramp at the same edge.
4. Stop the chart before adjusting and reapplying the pair. Save and reopen to confirm the overlap, gamma and relative brightness. Clearing either region or assigning both partners to the same screen suspends the ramp until the pair is restored.

If a projector disconnects, its saved screen assignment can stay in the pair and the surviving beam can still show its feathered edge. Switch to your rehearsed fallback look. For a single-projector recovery, stop playback, clear **Stage regions** on the surviving output, and rehearse the still with suitable geometry. After reconnecting, identify both physical screens again before restoring the pair. Screen numbers can change when cables move.

Rehearse with the show’s treatment and actual viewing conditions. Brightness correction cannot remove the combined black floor of two projectors. [QLab overlap and blend gamma](https://qlab.app/docs/v5/video/video-output/#understanding-regions)

## Audition a layer blend

Route two pictures to a spare output, with a colored still above a picture that includes both gray and colored detail. Start both cues, select the foreground and open **Layer → Blend Mode**. Compare **Linear Burn** and **Pin Light**, then try **Hue**, **Saturation**, **Color** and **Luminosity** to choose how the two pictures contribute to the result. Lower the foreground opacity to see more of the picture underneath. The **Saturation** slider adjusts the foreground before the blend, and the **Saturation** blend mode uses its resulting color strength for the overlap. Save and reopen to check your choice. [QLab blend modes](https://qlab.app/docs/v5/video/blend-modes/) · [W3C component blend formulas](https://www.w3.org/TR/compositing-1/#blendingnonseparable)

### Fill a shape with another cue

On a spare output, run a PNG with a transparent cutout or margin as an **Image** cue on a lower layer. To preserve the PNG’s transparent edges, turn on **Continuous still rotation** in **Geometry** and leave the angle at zero. Put a larger colored Image or Video above it and set that cue’s **Blend Mode** to **Source Atop**. The upper cue fills the shape where the lower picture has coverage. Lower the fill opacity to reveal the original shape color. Stop the lower cue to check that its fill disappears. Any other pictures below the fill also contribute to its key, so isolate the pair on the output during rehearsal. [QLab Source Atop](https://qlab.app/docs/v5/video/blend-modes/#compositing-blends)

## Make one video layer monochrome

1. Route two Image cues to a spare output. Use a still with prominent colors for the foreground and choose a higher layer number for it. Leave a part of the background visible by scaling down the foreground in **Geometry**. Start both cues.
2. In the foreground cue’s **Layer** tab, choose **Monochrome**. Its colors turn to grey while the background keeps its color. Use **Saturation** to add some color back, or go past 100% to intensify it. **Reset saturation** restores the original color.
3. Fade the foreground to 50% brightness. After the Fade completes, change only **Saturation**. The foreground should keep the Fade’s partial opacity. Try **Multiply** with visible background detail to check both layers together.
4. Save and reopen the show, then start the two cues again. Check the foreground’s saved saturation and starting opacity in **Layer**. Repeat on a Video or a live Camera cue if that is what the show uses.

Inkue applies saturation to the selected Video, Image, or Camera cue before blending it into the stack. The cue’s picture transparency and audio follow their own settings. For a QLab migration, compare the result on the original display and mark any effect ordering, automated changes, or plug-ins that need to be rebuilt and checked separately. [QLab Video FX](https://qlab.app/docs/v5/video/video-cues/) · [QLab Fading Video](https://qlab.app/docs/v5/video/fading-video/)

### Adjust brightness, contrast and hue on one layer

1. Start a Video, Image, or Camera cue over a second visual cue. Open **Layer → Color Controls** on the foreground. Use **Picture brightness** to lift or darken the source and **Contrast** to separate or flatten its light and dark values. Check the result on the stage or stage monitor.
2. Set **Hue angle** to 120° on a red test picture to see it turn green. Change **Saturation** to adjust how much color comes through. Compare the underlying cue and any visible transparent edges while you make these adjustments.
3. Save and reopen to recall the corrected picture. In rehearsal, choose **Reset color controls** to restore brightness 0, contrast 1×, hue 0° and saturation 100%.

When moving a show from QLab, copy the Color Controls settings into the corresponding Inkue controls. Rehearse the look on the target display. Inkue rotates the source in HSV and applies the controls before the blend mode. Set the Fade destinations separately in the Fade inspector. [QLab Color Controls parameter ranges](https://qlab.app/docs/v5/scripting/parameter-reference/#video-effects)

### Animate the picture's color controls

1. Start a colored still or movie. Target it with a two-second Fade and leave **Crossfade** set to **None**. Under **Visual**, turn off **Fade brightness** to keep opacity steady. Turn off **Fade volume** to keep the soundtrack steady.
2. Enable **Fade picture brightness**, **Fade contrast** and **Fade hue angle**. Try destinations of −0.2, 1.5× and 120°. Start the Fade and check the three changes on the stage. Pause and resume it to rehearse the transition. Hue passes through the intervening signed angles in numeric order.
3. After the Fade finishes, adjust only **Picture brightness** in the target's **Layer → Color Controls**. Select the Fade again and choose **Revert Picture Fade**. Contrast and hue return to the values they had when the Fade started. The manual brightness edit stays live.
4. Stop and restart the picture to check its saved starting settings. Combine color changes with saturation or geometry in the same Fade when they share the same timing. [QLab Fading Video](https://qlab.app/docs/v5/video/fading-video/)

### Fade the foreground to monochrome

1. Start the foreground and background from the exercise above. Set the foreground’s saved saturation to 100%.
2. Add a four-second Fade and target only the foreground cue. Leave **Crossfade** set to **None**. In **Visual**, enable **Fade saturation**, set **Destination saturation (%)** to zero and turn **Fade brightness** off. Use a linear curve for the first run.
3. GO the Fade. Pause it halfway to hold a partially coloured picture and resume to finish at grey. The background keeps its colour. Run a second Fade to 150% and watch it start at the grey value left on stage.
4. Try **Reset saturation** in the foreground’s **Layer** tab during a repeat. It takes over colour, while any position or scale changes programmed on that Fade keep running. An opacity or blend edit alone leaves the saturation Fade in control. Stop the picture and GO it again to restore its saved saturation.

Saturation animates one live Video, Image or Camera layer. Use the same Fade to combine it with position, scale or brightness. Turn **Fade volume** off to preserve a Video cue’s soundtrack level during a colour-only Fade. Rebuild and rehearse the colour change when migrating a QLab effect fade. [QLab Fading Video](https://qlab.app/docs/v5/video/fading-video/)

## Rotate a still around its centre

1. Add an Image cue with a plain, square-pixel PNG or JPEG. Use a picture twice as wide as it is high, with a distinct mark just right of centre. Put a coloured background on a lower layer of the same output.
2. In the foreground cue’s **Geometry**, choose **Fit**, enable **Continuous still rotation**, and set **Rotation** to 0°. Give the cue a GO. Move its **Position X** to 0.35 so the mark goes off the right side on a similar wide output.
3. Set **Rotation** to about 59°. The mark can roll back into view while the centre stays put. Type a decimal angle or use the slider. Compare the still’s upper edge, its corners and the background layer.
4. Crop the marked edge and rotate again. The crop follows the picture. Save and reopen the show, GO the cue and check the same angle and crop. Clear the option and restart the cue to use its previous Geometry mode.

The mode is chosen when the cue loads. Fit, Fill and Stretch establish the image size before the turn. Pan, scale, crop and fractional rotation can be edited live. Use a square-pixel PNG or an 8-bit greyscale or RGB JPEG with its intended orientation baked in. JPEGs can have no EXIF orientation or Orientation 1. Centre rotation checks the JPEG markers before revealing the image and supports files up to 256 MiB. If the desk prompts you to bake the orientation, open the file in a photo editor, apply its EXIF rotation or flip, export a local rehearsal copy and reassign the Image cue. Moving sources and 3D orientation need separate support. [QLab video geometry](https://qlab.app/docs/v5/video/video-cues/)

### Turn a still around an edge

1. Use a cropped PNG or JPEG with a clearly marked left edge. Enable **Continuous still rotation** and start it at 0°. Enter 0 for **Pivot X** and 50 for **Pivot Y**. The picture stays in its original position at 0°.
2. Enter 90° in **Rotation**. Check that the midpoint of the cropped left edge stays put as the picture turns around it. Enter 0° again to save that starting angle, then GO a Rotation Fade to 90°.
3. Stop and give the Image a fresh GO. Verify that its saved 0° angle returns with the edge pivot ready for the next turn. Choose **Centre pivot** to restore the default.

The pivot is measured from the left and top of the picture after crop. It controls rotation for these stills. Pan and scale keep their existing controls. QLab anchors also apply to its translation, scaling and 3D geometry. [QLab Video Geometry settings](https://qlab.app/docs/v5/video/video-cues/#the-geometry-tab)

### Fade the angle of a still

1. Use a wide PNG or JPEG with a short line pointing up from its centre. Enable **Continuous still rotation** in **Geometry**, set its angle to 350.5°, and GO the Image on a spare output.
2. Add a Fade that targets that single Image. Set **Crossfade** to **None**, and clear **Fade brightness**. In **Position, scale and rotation**, enable **Fade rotation around pivot** and enter 10.5°. Choose a four-second straight curve and leave **Stop targets when the fade ends** clear.
3. GO the Fade. About halfway through, the line should pass upright along the shorter turn. Pause the Fade, confirm the image holds, then resume. Stop the Fade mid-turn on another pass. The next Fade starts at the held angle, and a fresh GO of the Image restores 350.5°.
4. During a fresh Fade, edit the Image's angle in **Geometry**. The live edit takes control. Save and reopen to confirm the Image angle and the Fade destination remain separate.

The rotation uses the chosen curve for the direction it travels. An exact half turn to an absolute angle goes clockwise. Start the Image and confirm its first frame before calling the Fade. Position and scale can share the Fade, and saturation follows its own ownership. Continuous rotation on Video and Camera cues and 3D rotation still need separate support. [QLab geometry fades](https://qlab.app/docs/v5/video/fading-video/#the-geometry-tab)

For a full revolution, choose **Specified turn** in the Fade’s **Rotation motion** field and enter 360°. Use −360° for a counter-clockwise revolution or 720° for two clockwise turns. The turn starts from the Image’s live angle around its saved pivot, and the Fade duration determines how long it takes. Try 450° twice to see each GO add a full turn and a quarter turn. A fresh GO of the Image goes back to its saved angle. Choose **Shortest path to angle** for an absolute destination. [QLab Video Fade rotation](https://qlab.app/docs/v5/video/fading-video/#the-geometry-tab)

## Change the speed of a silent movie

1. Use a silent local MP4, MOV or MKV with visible motion and at least 20 seconds of footage. Add it as a Video cue. Use manual continuation and clear trims, loops, slices, the visual end fade and **Hold last frame**. In **Time**, enable **Allow silent movie speed fades** and start the picture.
2. Create a four-second Fade targeting that Video. Enable **Fade silent movie speed** and enter 2 for **Destination speed (×)**. GO the Fade and compare the movement and file-time counter before and after the ramp.
3. During a repeat, select the Fade and press **P** to pause just that cue. The movie keeps playing at the speed the Fade reached. Press **]** with the Fade selected to resume, or open the Video cue’s **Time** tab and choose **Reset live speed to 1×** to take over speed.
4. Stop the Video and GO it again. It starts at 1× and ends when playback reaches the actual end of the file. Save and reopen to confirm the opt-in and the Fade destination.

The speed change applies to one active picture. Inkue confirms that the loaded movie has a video track and no soundtrack before it accepts a speed Fade. A Matroska file must play as a single linear movie. Save a flattened rehearsal copy if the original uses ordered chapters or linked segments. The time display follows progress through the file. Rebuild the effect in Inkue when transferring a show. Movies with synchronized audio need a separate rehearsal workflow. [QLab video fades](https://qlab.app/docs/v5/video/fading-video/)

## Check the stage picture from the desk

1. Route a still with an obvious top edge and a transparent area to the output you want to check. GO a second still behind it. Leave the output showing on a spare display.
2. Open **Cue monitor → Stage snapshot**, select that output by name, and choose **Capture frame**. Compare the stacked pictures, corner pins, and masked areas with the spare display. Confirm the top edge appears at the top in both places.
3. Choose **Set reference** to keep that still. Adjust a corner pin or mask, then choose **Retake frame**. Compare the edges in **Reference frame** and **Latest capture**. After Hard Stop, retake again to compare the blackout against the reference.
4. Choose **Replace reference** to use the latest still as the new baseline or **Clear reference** to return to one image. Changing the output or workspace, or leaving the Stage snapshot view, clears both images. In still mode, each new picture requires a capture.

### Watch an output during rehearsal

1. Open **Cue monitor → Stage snapshot**, choose a visible output, and select **Start live preview**. Call a color change or bring in a title. Compare the thumbnail with what is on the stage.
2. Select **Set reference** to keep the current frame beside the updating preview. Select **Stop live preview** to hold the latest thumbnail. Use **Retake frame** when you want one new still.
3. Hide the output or change the selected output. A hidden output ends preview at the next capture. Changing output or closing the view ends preview immediately. Select **Start live preview** again when you want to watch the new output.

The live preview uses a thumbnail up to 320 × 180 pixels and up to 24 frames per second. It reads finished stage frames, including masks and blackout, and displays the newest available frame. The stage output stays in charge of presentation timing. Use the physical display to verify frame timing, latency, or fine detail. [QLab video output monitors](https://qlab.app/docs/v5/tools/monitor-windows/#video-output-monitor-windows)

### Put the stage monitor on another display

1. In **Cue monitor → Stage snapshot**, select a visible output and choose **Open monitor window**. Move the window to the operator display and resize it. The selected output's live preview starts in the separate window.
2. Return to the main desk and continue calling cues. To bring the same monitor forward, choose **Open monitor window** again for that output. Select a different output to open its own monitor.
3. Use **Stop live preview** to hold the latest picture, or **Set reference** to keep a comparison still. Closing the monitor releases its preview. Reopen the monitor after switching workspaces.

Each request reads a reduced copy of the final rendered picture, including the on-output text, regions, masks, and blackout. It works on an already visible output. Test the signal path, projector optics, and audience-side brightness on the physical display.

## Bringing over a QLab show

Inkue reads the QLab viewport or surface route, the cue’s opacity and explicit layer, and the Video cue’s hold-last-frame flag. An absolute QLab opacity fade becomes a brightness target. Check its curve against the reference show. A cue that combines opacity with a moving geometry fade still needs a manual review.

A Video cue can carry a separate QLab audio mixer. When the show uses sound from the clip, compare its starting level and channel assignments with the reference desk. Check any mute or solo selections flagged in the import report with the output monitors before enabling the house feed.

Inkue preserves finite play counts and **Infinite loop** on imported Audio and Video cues. For a looping scene, start the imported clip on a rehearsal output, watch or listen through its first repeat, then Stop it and check the end of the scene. If the import report flags a slice program, compare its markers and loop counts before arming that cue. [QLab play count and infinite loops](https://qlab.app/docs/v5/audio/audio-cues/#play-count-and-infinite-loops)

QLab’s Custom mode sizes a picture against its natural pixels. Inkue’s inspector sizes it against the fitted output frame. Import notes record source position and scale for that comparison. They also identify pixel crops, unsupported blend modes and projector regions that need alignment. Film the reference cue on the original desk when a pixel match matters. Consult the [QLab Video Cue inspector](https://qlab.app/docs/v5/video/video-cues/) and [QLab video output guide](https://qlab.app/docs/v5/video/video-output/).

Native NDI and Syphon patching, QLab video-effect chains and animated three-dimensional transforms have no verified drop-in replacement here. A show using them needs a separate design and playback test before the operator can sign off on the move.

### Compare an imported picture at the stage edges

1. In a QLab practice workspace, use a wide test image on a square stage. Put a circle in the centre and a marker near each side. Give three independent Video cues the same file. In QLab 5, set **Fill Stage** to **Fit**, **Fill** and **Stretch** respectively. For a QLab 4 show, use **Full Surface** with **Preserve Aspect Ratio** checked and unchecked.
2. In Inkue, choose **File → Import QLab Workspace**, assign the intended test display, and relink the file if needed. The **Geometry** inspector shows the imported style. QLab 4 with the checkbox on becomes **Fit**. With it off, the cue becomes **Stretch**.
3. GO each cue separately. With **Fit**, both markers and a round circle are visible, with unused space above and below. With **Fill**, the sides crop and the circle stays round. With **Stretch**, both markers stay visible and the circle becomes taller. Stop each cue before starting the next.
4. Save and reopen the Inkue show and compare one screenshot for each style on the intended output. If a report flags an unknown sizing setting, check the original cue before relying on the imported fit.

[QLab 5 Fill Stage styles](https://qlab.app/docs/v5/video/video-cues/#the-geometry-tab) · [QLab 4 Full Surface](https://qlab.app/docs/v4/video/video-cues/#display--geometry)

### Prepare a video before its entrance

Add a **Load** cue, target the Video cue and run Load during standby. The desk opens the clip in a spare output layer and holds its paired audio paused. You can keep the audience display closed, blacked out or showing an earlier cue. The target appears paused while prepared. Its clock starts when you give it GO or Start. The programmed pre-wait still runs before the picture and audio are released.

If the video layers are all occupied, the live pictures continue and Show Health reports that Load could not prepare the target. Run Load again after a layer becomes free. Hard Stop clears prepared content. Rehearse the entrance on the intended output and record its measured latency.

## Finish a silent movie loop at its natural end

1. Choose a local silent MP4 or MOV with a recognisable ending. Add it to a Video cue on a rehearsal output and turn on **Loop** in **Time** and choose **Loop infinitely**. Keep continuation manual and leave the trim, slices, visual end fade and **Hold last frame** clear.
2. Add a top-level Devamp cue. Choose **Whole cue** in **Release scope** and select the Video under **Targets**. Leave the Devamp’s Pre-Wait at zero and its Continue mode at **Do Not Continue**.
3. Start the Video and watch it repeat. GO the Devamp during the middle of the next pass. Watch the picture play to its natural end and clear its own layer while any other picture stays in place. A request right at the loop boundary can reach the decoder after the next pass has begun.
4. Start the same Video again and watch it repeat. Each GO uses the saved Loop setting. Try the Devamp when the target is stopped, paused or disarmed, and with an MP4 that contains a soundtrack. Inkue refuses those requests and keeps the target in its current state.

This exercise covers one already-running silent Video on Linux with libmpv 0.39 or later. Inkue verifies the file’s tracks even when its audio would otherwise be muted. The Devamp ends the current pass and leaves the next cue for a manual GO. When migrating a Devamp that releases a soundtrack loop or starts a successor at the boundary, rehearse those timings in the reference show. [QLab Devamp cue manual](https://qlab.app/docs/v5/other-cues/devamp-cues/#slice-or-cue)
