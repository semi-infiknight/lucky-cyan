# Programming a lighting look

You can program Light cues with fixture names and levels, review the result at your desk and connect your lighting rig for rehearsal. A Light cue holds changes to specific parameters. On GO the DMX engine starts the cue’s snap or fade together, with one start time for its individual and grouped parameters.

## Build and review a look offline

1. Open **DMX** on the transport bar in programming mode. Select **Patch**. Start with an empty **Universe outputs** list or disable the outputs on your rehearsal copy.
2. Under **Fixtures (patch)** choose a dimmer, name it `1`, set its logical universe and start address, and add it. Add two more dimmers named `2` and `3` at free addresses. Resolve patch conflicts.
3. Under **Groups**, select all three fixtures and create a group called `Front`.
4. Close the DMX panel and add a **Light** cue. Select it, open the Light inspector, and expand **Program with light commands**.
5. Set **Default parameter** to **intensity** and **Numeric levels** to **Percent**. Enter the following lines.

```text
Front = 70
2 = pass
3.intensity = 25
```

6. Select **Preview look**. The table should show fixture 1 at 70% and fixture 3 at 25%. Fixture 2 keeps its preceding level. In this example `pass` removed fixture 2 from the proposed look.
7. Select **Replace cue targets** to write the entire reviewed look to this cue. It replaces the existing parameter targets. Set **Fade time** and **Curve**, save the workspace, then reopen the cue to check its targets.

Preview and Replace update cue data only. The dashboard and test channel in **Live controls**, the patch’s **Identify** button and the **Blackout** control operate the live engine immediately. **Output monitor** shows the current universe values.

## Reuse a fixture profile in another workspace

Use the machine’s **Fixture library** for profiles you want available whenever you open the patch. Open a patched fixture’s **Profile** and choose **Add saved profile to library**. To keep a profile file you just opened, choose **Add imported profile to library** instead.

In another workspace, select the definition under **Fixture library** in the patch picker. Choose the universe and starting address, then select **Patch fixture**. Inkue checks the address and gives the fixture its own ID and a copy of the profile. To retire a library entry, select it and choose **Remove from library…**. Existing shows keep their patched copies.

Use a portable file to take the profile to another machine.

1. In a rehearsal copy, open **DMX → Patch** and add **Moving Head 16-bit (8ch)**. Open **Profile**, give it a distinct model and mode, and save the profile. Reopen **Profile** and choose **Export saved profile…**. Save `Tour moving head.inkue-fixture`.
2. Open a second rehearsal workspace. Choose **DMX → Patch → Import profile…** and select the file. Check the selected name, mode and footprint. Choose a free universe and start address, then use **Patch fixture**. Import and patch prepare fixture data without sending DMX. The selected profile can be patched again at another free address.
3. Open the new fixture’s **Profile** and verify the roles and both addresses on each 16-bit row. Try patching the same profile at an already used address to see the conflict. Save and reopen the workspace to confirm the new fixture carries its own copy of the definition. The format carries channel roles, defaults, and coarse and fine slots relative to the fixture start address. [QLab Light Library](https://qlab.app/docs/v5/lighting/light-library/)

## Patch separated 16-bit channels

1. In an offline workspace, open the **Profile** for **Moving Head 16-bit (8ch)** patched at address 101. On the Pan row, enter **9** in **Fine slot**. Slot 1 is the fixture's first address. The row shows coarse address 101 and fine address 109. A blank field means the next slot after coarse.
2. Save the profile. In the Dashboard, move Pan to a quarter turn and check addresses 101 and 109 in the output monitor. Adjust another parameter at its own address, then capture the look into a Light cue and play it back with a fade. The patch reserves through address 109. Choose a different fixture start when that span would cross address 512.
3. Save the workspace and reopen it using an Inkue build that supports explicit fine slots. Check the same two addresses after moving or importing the profile into another show.

## Program a CMY filter look

1. Disconnect the venue outputs or use a rehearsal copy with universe outputs disabled. In **DMX → Patch**, add the **CMY (3ch)** template for an offline example at universe 4, address 201. For a venue fixture, compare its profile with the manufacturer’s chart. Assign one explicit **cyan**, **magenta** and **yellow** role each to the fixture’s subtractive channels and save the profile.
2. Add a Light cue and target the fixture. In **Subtractive color (CMY)**, enter **25%** Cyan, **50%** Magenta and **75%** Yellow, then choose **Apply CMY to cue**. Set a one-second fade. Save and reopen the show to check the look. The editor saves only the three selected levels and leaves any dimmer target as authored.
3. Run the cue and inspect the resulting look in **DMX → Output monitor**. The 8-bit template ends at **64**, **128** and **191** on addresses 201–203. For live adjustment, use **DMX → Dashboard → Apply to live output**. Check that the dimmer level and unrelated parameters stay put.

The color swatch uses the complementary RGB values as a preview. The actual color depends on the fixture’s filters and light source. The combined editor requires three explicit, nonoverlapping CMY roles. Individual channels remain available from the native Light command editor, including expressions such as `Wash.magenta = 50%`. Confirm channel direction on the fixture before using the venue output. Virtual CMY mappings in QLab archives and color calibration require separate work. [QLab Light Patch Editor](https://qlab.app/docs/v5/lighting/light-patch-editor/) · [QLab Light Dashboard](https://qlab.app/docs/v5/lighting/light-dashboard/)

## Program a native HSI look

1. With universe outputs disabled, add **HSI (3ch)** in **DMX → Patch** at universe 5, address 401. To use a venue fixture, match its physical channel chart and save one **hue**, one **saturation** and one **HSI intensity** role in **Profile**. Use **intensity** for a separate master dimmer.
2. Add a Light cue, target the fixture and open **Native color (HSI)**. Enter **120°** Hue, **60%** Saturation and **40%** HSI intensity. Select **Apply HSI to cue**, choose a fade time, and save and reopen the workspace. The three values should persist.
3. Run the cue. In **DMX → Output monitor**, the 8-bit template should finish at **85**, **153** and **102** on addresses 401–403. To set the same look from **DMX → Dashboard**, enter those degrees and percentages, then choose **Apply to live output**. The HSI editor leaves a separate master dimmer’s cue target or live level as set.

The swatch approximates the fixture’s color on screen. Hue runs from 0° to 360° and both endpoints look the same in the preview. They map to opposite ends of the DMX channel, and fades interpolate those channel values through the selected curve. Enter hue as a percentage of that full range when using the Light command editor, for example `Wash.hue = 50%` for 180°. Confirm the fixture’s hue range, saturation direction and intensity response before connecting outputs. Importing QLab virtual HSI and HSV color maps requires rebuilding the fixture profile and reviewing its cue targets. [QLab Light Patch Editor](https://qlab.app/docs/v5/lighting/light-patch-editor/) · [QLab Light Dashboard](https://qlab.app/docs/v5/lighting/light-dashboard/)

## Program a native HSV look

1. With universe outputs disabled, add **HSV (3ch)** in **DMX → Patch** at universe 7, address 101. On a real fixture, use its chart to assign **hue**, **saturation** and **HSV value** in **Profile**. Choose one color model for each hue and saturation pair and use **intensity** for a separate master dimmer.
2. Add a Light cue, target the fixture and open **Native color (HSV)**. Enter **240°** Hue, **50%** Saturation and **75%** HSV value. Choose **Apply HSV to cue** and set the fade time. Save and reopen the show to check the saved mix.
3. Run the cue and open **DMX → Output monitor**. The 8-bit example should finish at **170**, **128** and **191** on addresses 101–103. To adjust a fixture live, use the same HSV controls in **DMX → Dashboard** and choose **Apply to live output**. The Dashboard sends the three physical levels together.

The swatch uses HSV color math for preview. The fixture receives the authored hue, saturation and value on its own channels. In the command editor, enter `Wash.hsv_value = 75%` to address the value channel explicitly. Confirm the manufacturer’s scaling and response on the venue rig. [QLab Light Patch Editor](https://qlab.app/docs/v5/lighting/light-patch-editor/)

## Program one color across a fixture group

1. With universe outputs disabled, patch two **CMY (3ch)** fixtures on universe 6 at addresses 101 and 111. In **DMX → Patch → Groups**, create `Window wash` and add both fixtures.
2. Add a Light cue and use **Add group** in its inspector to choose `Window wash`. In **Subtractive color (CMY)** enter **20%** Cyan, **40%** Magenta and **60%** Yellow. Select **Apply CMY to cue**, set a one-second fade, and save the workspace. A group of explicit HSI or HSV fixtures uses the same steps with the fields for its common color model.
3. Reopen the show, run the cue and check **DMX → Output monitor**. Both address ranges 101–103 and 111–113 should reach **51**, **102** and **153** together. A separate dimmer keeps its earlier level unless the cue has a dimmer target.
4. In the offline copy, add an incompatible fixture to the group. Return the playhead to the Light cue and press GO. The inspector flags the patch and GO refuses the look. Remove the incompatible member, then retry.

A group color becomes part of the cue after you select Apply. Each member needs a full, explicit triplet for the chosen model. Inkue resolves current members and physical slots together at GO, including both slots of any 16-bit parameter. Review affected cues after changing group membership or a fixture profile. [QLab lighting command language](https://qlab.app/docs/v5/lighting/lighting-command-language/) · [QLab Light Dashboard](https://qlab.app/docs/v5/lighting/light-dashboard/)

## Adjust a fixture group live

1. Leave the rehearsal universe outputs disabled and use the `Window wash` group from the previous exercise. In **DMX → Live controls**, change Cyan on one fixture in the individual Dashboard controls and select **Apply to live output**.
2. Expand **Fixture group colors** in the Dashboard. The group shows **Mixed** in the Cyan field when its members have different values. Enter **20%** Cyan, **40%** Magenta and **60%** Yellow, then select **Apply to live output** on the group.
3. Confirm that both fixtures’ individual controls update and that **DMX → Output monitor** shows **51**, **102** and **153** for each fixture on universe 6. Test a fixture’s separate dimmer before and after the group color Apply to verify that its level holds.

The Dashboard offers group controls for compatible CMY, HSI and HSV fixtures. Fill any mixed fields before applying. If a group or its physical patch changes while the Dashboard is open, use **↻ Live** to load the current membership, addresses and levels before retrying. Group Apply sets live levels. Use **Capture live state** in the Light cue inspector to save the live look as fixture targets. [QLab Light Dashboard](https://qlab.app/docs/v5/lighting/light-dashboard/)

## Move a QLab Light cue

Choose **File → Import QLab Workspace**. Every Light cue imports as a disarmed Light cue carrying its cue number, source data, waits, continue mode and fade duration. Check its row in the conversion report. Review the **Lighting patch** section of the report, then check fixture and group labels under **DMX**. In the Light inspector, **Review imported light commands** displays a command editor. Archived `commandText` prepopulates this field when available. **Original QLab cue data** shows the preserved decoded cue, including its original fade data. When the workspace provides another representation, use that data and the original QLab inspector to rebuild the commands.

Choose the original parameter and units before previewing. QLab lets the fixture definition determine both. Inkue uses the default parameter and numeric-unit controls for this sheet. Use explicit parameter suffixes such as `.intensity` and explicit percentages such as `50%` where a cue mixes defaults. Some edits require adjusting the names or converting raw values to the corresponding percentage first.

The compiler processes lines from top to bottom. It accepts fixture and group labels, comma-separated selections, ascending numeric ranges, bracketed numeric selections, named parameters, numeric levels, `home` and `pass`. A group or range expands into individual fixture parameters at review time. Reapply the sheet after patch membership changes. A missing fixture, ambiguous name, invalid level, unknown parameter on an individual fixture, or unsupported expression produces a line-specific error. All lines must resolve before a replacement can be applied.

QLab cue pulls such as `Front = cue A`, proportional pulls, and automatic collation of earlier looks require the operator to reconstruct the intended values. Check QLab’s original fade shapes against Inkue’s curve choices. Derived presets, collation and original output patch addresses require a rehearsal. Compare the resulting look on the venue rig. [QLab lighting command language](https://qlab.app/docs/v5/lighting/lighting-command-language/) · [QLab Light cues](https://qlab.app/docs/v5/lighting/light-cues/)

## Transfer a fixture patch offline

1. Open **File → Import QLab Workspace**. In the conversion report, check **Lighting patch** for source USB ports, their assigned logical universes and fixtures that need rebuilding. **Save conversion report…** keeps this list with your rehearsal notes.
2. Open **DMX → Patch**. Check each fixture’s label, universe and one-based start address against the QLab patch. Each distinct supported device and port gets its own logical universe. Universe outputs start empty.
3. Choose **Profile** beside a fixture. Check **Manufacturer**, **Model / definition** and the per-address channel list. QLab often includes the personality in its definition name. Inkue preserves that complete name. Enter a separate **Mode** label if that helps the crew match the fixture menu.
4. Check each **Control role** against the fixture chart. Assigning the same RGB role to several channels gangs those channels on that fixture’s color picker. A complete set of red, green and blue roles enables the picker in the Light inspector and live Dashboard. White, amber and UV remain separately adjustable. **Save profile** writes the mapping to the workspace; **Cancel** discards the draft.
5. Add a disarmed Light cue and preview a command addressing one imported scalar, for example `SixPar.red = 50%`. Check the preview against the profile. Keep venue outputs disconnected until you have verified the patch.

The QTableTop QLab 4 workspace provides a concrete example. Its **SixPar** imports with model **SIXPAR series - 6 channel**, manufacturer **Elation**, start address **1**, and red, green, blue, white, amber and UV on addresses **1–6**. Its archived ENTTEC USB port becomes a logical universe in Inkue. The operator chooses the eventual venue output.

This transfer supports patched instruments on the archived QLab USB backend with one-byte scalar percentage channels and zero home levels. QLab RGB color definitions that name their scalar outputs supply their color roles. Inkue’s RGB picker drives red, green and blue. QLab’s automatic white/amber mix and virtual default parameter require separate review. An unrecognized backend, two-byte parameter, unsupported virtual parameter or unresolved color output sends the entire instrument to the report for manual patching. Groups, MIDI triggers, unused library definitions and physical interface assignments require separate setup. [QLab lighting command language](https://qlab.app/docs/v5/lighting/lighting-command-language/)

## Rehearse on the rig

Configure the intended sACN or Art-Net universe output and destination in the DMX panel. Compare universe, channel, parameter width, fixture personality and addresses with the venue patch. After reviewing the saved cue, clear **Disable this cue** in Basics when ready. Run the sequence through GO, verify its fixtures and fade, then verify blackout. Show Mode keeps the live output monitor and blackout control available. Return to programming mode to change the patch or test individual channels. [QLab lighting tutorial](https://qlab.app/docs/v4/tutorials/adding-lights-and-making-light-cues/)
