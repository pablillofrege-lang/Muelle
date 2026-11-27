use super::*;

impl MenuBar {
    pub(super) fn toggle_bluetooth_panel(&mut self, gfx: &Gfx) -> Result<()> {
        if let Some(mut look) = self.look.clone() {
            look.panel = toggle_panel(self.panel, Island::Bluetooth);
            self.apply_look(gfx, look)?;
        }
        Ok(())
    }

    pub(super) fn draw_bluetooth_panel(
        &mut self,
        gfx: &Gfx,
        look: &Look,
        layers: &VisualCollection,
        width: f32,
        bar_height: f32,
        trigger: Span,
    ) -> Result<()> {
        let Some(devices) = look.bluetooth_devices else {
            return Ok(());
        };
        let px = |logical: f32| (logical * look.scale).round();
        self.panel_span = bubble_panel_span(trigger, width, px(316.0));
        let geometry = IslandGeometry::new(self.panel_span, bar_height + px(PANEL_GAP), look);
        let height = px(look.panel_height());
        let radius = px(PANEL_RADIUS);
        let theme = Theme::for_popup(look.light, look.glass);
        let content = popup_surface(gfx, theme, layers, geometry, height, radius)?;
        self.capsule_layout.panel =
            RegionPart::rounded(self.panel_span, geometry.top, height, radius);
        let ink = if look.light { Ink::Dark } else { Ink::Light };
        let (title, disconnected, connected, dualsense) = strings::bluetooth_devices(look.language);
        let right = geometry.width - px(14.0);
        add_panel_text(
            gfx,
            &content,
            title,
            px(14.0),
            px(12.0),
            look.scale,
            13.0,
            true,
            ink,
            1.0,
        )?;

        let mut center = px(50.0);
        for model in BluetoothModel::ALL {
            let status = devices.status(model);
            if model == BluetoothModel::AirPods && status.is_none() {
                continue;
            }
            let status = status.unwrap_or_default();
            let name = match model {
                BluetoothModel::Sony => "Sony WH-1000XM5",
                BluetoothModel::AirPods => "AirPods Max",
                BluetoothModel::DualSense => dualsense,
            };
            let opacity = if status.connected { 1.0 } else { 0.66 };
            let value = match status.percent {
                Some(percent) => format!("{percent} %"),
                None if status.connected => connected.to_owned(),
                None => disconnected.to_owned(),
            };
            let label = gfx.text_brush_sized(&value, look.scale, 12.0, false, ink)?;
            let place = |brush: &LabelBrush, left| -> Result<()> {
                add_visual(
                    &gfx.compositor,
                    &content,
                    &brush.brush.cast()?,
                    left,
                    (center - brush.height / 2.0).round(),
                    brush.width,
                    brush.height,
                )?
                .SetOpacity(opacity)?;
                Ok(())
            };
            let mut value_left = right - label.width;
            place(&label, value_left)?;
            if let Some(percent) = status.percent {
                let battery = gfx.battery_brush(percent, false, false, look.scale * 0.75, ink)?;
                value_left -= battery.width + px(6.0);
                place(&battery, value_left)?;
            }
            let icon = gfx.glyph_brush(&[(bluetooth_icon(model), 1.0)], look.scale, ink)?;
            place(&icon, px(14.0))?;
            let name_left = px(40.0);
            let name = gfx.text_brush_fitted(
                name,
                look.scale,
                12.0,
                false,
                ink,
                (value_left - name_left - px(8.0)).max(1.0),
            )?;
            place(&name, name_left)?;
            center += px(27.0);
        }
        Ok(())
    }
}
