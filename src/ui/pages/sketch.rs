use crate::ui::app::{BerApp, Page};
use eframe::egui;
use egui::epaint::{Color32, Stroke};

pub struct SketchPage;

/// Drawing modes for the sketch canvas
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawMode {
    FloorPlan, // Draw building outline/floor plan
    Room,      // Draw individual rooms
    Wall,      // Draw walls
    Door,      // Place doors
    Window,    // Place windows
    Eraser,    // Erase strokes
}

impl DrawMode {
    pub fn label(&self) -> &str {
        match self {
            DrawMode::FloorPlan => "Floor Plan",
            DrawMode::Room => "Room",
            DrawMode::Wall => "Wall",
            DrawMode::Door => "Door",
            DrawMode::Window => "Window",
            DrawMode::Eraser => "Eraser",
        }
    }

    pub fn color(&self) -> Color32 {
        match self {
            DrawMode::FloorPlan => Color32::from_rgb(50, 50, 200),
            DrawMode::Room => Color32::from_rgb(100, 200, 100),
            DrawMode::Wall => Color32::from_rgb(80, 80, 80),
            DrawMode::Door => Color32::from_rgb(139, 69, 19),
            DrawMode::Window => Color32::from_rgb(135, 206, 235),
            DrawMode::Eraser => Color32::from_rgb(255, 100, 100),
        }
    }
}

/// A single stroke drawn by the user
#[derive(Debug, Clone)]
pub struct DrawStroke {
    pub points: Vec<egui::Pos2>,
    pub mode: DrawMode,
    pub is_closed: bool,
}

impl DrawStroke {
    pub fn new(mode: DrawMode) -> Self {
        Self {
            points: Vec::new(),
            mode,
            is_closed: false,
        }
    }

    pub fn add_point(&mut self, point: egui::Pos2) {
        self.points.push(point);
    }

    /// Check if this stroke forms a closed loop
    /// Returns true if start and end points are within epsilon distance
    pub fn check_closed(&mut self, epsilon: f32) -> bool {
        if self.points.len() < 3 {
            return false;
        }

        if let (Some(first), Some(last)) =
            (self.points.first().copied(), self.points.last().copied())
        {
            let distance = first.distance(last);
            if distance <= epsilon {
                self.is_closed = true;
                // Snap last point to first to truly close the loop
                if let Some(last_point) = self.points.last_mut() {
                    *last_point = first;
                }
                return true;
            }
        }
        false
    }

    /// Get bounding box of the stroke
    pub fn bounding_box(&self) -> Option<egui::Rect> {
        if self.points.is_empty() {
            return None;
        }

        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;

        for point in &self.points {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }

        Some(egui::Rect::from_min_max(
            egui::pos2(min_x, min_y),
            egui::pos2(max_x, max_y),
        ))
    }
}

/// State for the sketch canvas
pub struct SketchState {
    pub current_mode: DrawMode,
    pub strokes: Vec<DrawStroke>,
    pub current_stroke: Option<DrawStroke>,
    pub is_drawing: bool,
    pub closure_epsilon: f32, // Distance threshold for closing loops
    pub show_closure_feedback: bool,
    pub last_closed_stroke_idx: Option<usize>,
}

impl Default for SketchState {
    fn default() -> Self {
        Self {
            current_mode: DrawMode::FloorPlan,
            strokes: Vec::new(),
            current_stroke: None,
            is_drawing: false,
            closure_epsilon: 20.0, // 20 pixels
            show_closure_feedback: false,
            last_closed_stroke_idx: None,
        }
    }
}

impl SketchPage {
    pub fn render(ui: &mut egui::Ui, app: &mut BerApp) {
        // Header
        ui.horizontal(|ui| {
            if ui.button("← Back to Home").clicked() {
                app.navigate_to(Page::Home);
            }

            ui.add_space(20.0);
            ui.heading("Sketch Floor Plan");
        });

        ui.separator();
        ui.add_space(10.0);

        // Show feedback if a loop was just closed
        if app.sketch_state.show_closure_feedback {
            ui.colored_label(
                Color32::GREEN,
                "✓ Floor plan closed! Processing and simplifying lines...",
            );
            ui.add_space(5.0);
        }

        // Canvas area
        let canvas_size = egui::vec2(800.0, 500.0);
        let (response, painter) = ui.allocate_painter(canvas_size, egui::Sense::drag());

        // Draw canvas background
        painter.rect_filled(response.rect, 0.0, Color32::from_rgb(250, 250, 250));

        // Draw grid (optional - helpful for alignment)
        Self::draw_grid(&painter, response.rect);

        // Draw all completed strokes
        for (idx, stroke) in app.sketch_state.strokes.iter().enumerate() {
            Self::draw_stroke(&painter, stroke, false);

            // Highlight the last closed stroke
            if Some(idx) == app.sketch_state.last_closed_stroke_idx {
                if let Some(bbox) = stroke.bounding_box() {
                    painter.rect_stroke(
                        bbox.expand(5.0),
                        2.0,
                        Stroke::new(2.0, Color32::GREEN),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        // Draw current stroke being drawn
        if let Some(stroke) = &app.sketch_state.current_stroke {
            Self::draw_stroke(&painter, stroke, true);
        }

        // Handle drawing interaction
        if response.drag_started() {
            // Start a new stroke
            app.sketch_state.is_drawing = true;
            let mut new_stroke = DrawStroke::new(app.sketch_state.current_mode);
            if let Some(pos) = response.interact_pointer_pos() {
                new_stroke.add_point(pos);
            }
            app.sketch_state.current_stroke = Some(new_stroke);
            app.sketch_state.show_closure_feedback = false;
        }

        if app.sketch_state.is_drawing {
            if let Some(pos) = response.interact_pointer_pos() {
                // Add point to current stroke
                if let Some(stroke) = &mut app.sketch_state.current_stroke {
                    stroke.add_point(pos);
                }
            }
        }

        if response.drag_stopped() {
            // Finish the stroke
            app.sketch_state.is_drawing = false;

            if let Some(mut stroke) = app.sketch_state.current_stroke.take() {
                // Check if stroke is closed (for FloorPlan mode)
                let was_closed = if stroke.mode == DrawMode::FloorPlan {
                    stroke.check_closed(app.sketch_state.closure_epsilon)
                } else {
                    false
                };

                let stroke_idx = app.sketch_state.strokes.len();
                app.sketch_state.strokes.push(stroke);

                // If closed, send to processing engine
                if was_closed {
                    app.sketch_state.show_closure_feedback = true;
                    app.sketch_state.last_closed_stroke_idx = Some(stroke_idx);

                    // Send to app controller for processing
                    Self::process_closed_floor_plan(app, stroke_idx);
                }
            }
        }

        ui.add_space(10.0);
        ui.separator();

        // Mode selection buttons
        ui.horizontal(|ui| {
            ui.label("Drawing Mode:");
            ui.add_space(10.0);

            for mode in [
                DrawMode::FloorPlan,
                DrawMode::Room,
                DrawMode::Wall,
                DrawMode::Door,
                DrawMode::Window,
                DrawMode::Eraser,
            ] {
                let is_selected = app.sketch_state.current_mode == mode;

                let button = egui::Button::new(mode.label()).fill(if is_selected {
                    mode.color().linear_multiply(0.5)
                } else {
                    Color32::from_gray(240)
                });

                if ui.add(button).clicked() {
                    app.sketch_state.current_mode = mode;
                }
            }
        });

        ui.add_space(10.0);

        // Control buttons
        ui.horizontal(|ui| {
            if ui.button("Clear All").clicked() {
                app.sketch_state.strokes.clear();
                app.sketch_state.current_stroke = None;
                app.sketch_state.show_closure_feedback = false;
                app.sketch_state.last_closed_stroke_idx = None;
            }

            if ui.button("Undo Last").clicked() {
                app.sketch_state.strokes.pop();
                app.sketch_state.show_closure_feedback = false;
            }

            ui.add_space(20.0);

            // Closure epsilon slider
            ui.label("Snap Distance:");
            ui.add(
                egui::Slider::new(&mut app.sketch_state.closure_epsilon, 5.0..=50.0).suffix("px"),
            );
        });

        // Stats
        ui.add_space(10.0);
        ui.label(format!(
            "Strokes: {} | Current mode: {}",
            app.sketch_state.strokes.len(),
            app.sketch_state.current_mode.label()
        ));
    }

    /// Draw a grid on the canvas for alignment
    fn draw_grid(painter: &egui::Painter, rect: egui::Rect) {
        let grid_spacing = 50.0;
        let grid_color = Color32::from_gray(230);

        // Vertical lines
        let mut x = rect.min.x;
        while x <= rect.max.x {
            painter.line_segment(
                [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
                Stroke::new(0.5, grid_color),
            );
            x += grid_spacing;
        }

        // Horizontal lines
        let mut y = rect.min.y;
        while y <= rect.max.y {
            painter.line_segment(
                [egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)],
                Stroke::new(0.5, grid_color),
            );
            y += grid_spacing;
        }
    }

    /// Draw a stroke on the canvas
    fn draw_stroke(painter: &egui::Painter, stroke: &DrawStroke, is_current: bool) {
        if stroke.points.len() < 2 {
            return;
        }

        let color = if is_current {
            stroke.mode.color().linear_multiply(0.7)
        } else {
            stroke.mode.color()
        };

        let stroke_width = if is_current { 3.0 } else { 2.5 };

        // Draw the stroke as connected line segments
        for window in stroke.points.windows(2) {
            painter.line_segment([window[0], window[1]], Stroke::new(stroke_width, color));
        }

        // If closed, draw a filled shape
        if stroke.is_closed && stroke.points.len() >= 3 {
            let fill_color = color.linear_multiply(0.1);
            painter.add(egui::Shape::convex_polygon(
                stroke.points.clone(),
                fill_color,
                Stroke::new(stroke_width, color),
            ));
        }

        // Draw start point indicator
        if let Some(first) = stroke.points.first() {
            painter.circle_filled(*first, 4.0, Color32::GREEN);
        }

        // Draw end point indicator (if not closed)
        if !stroke.is_closed {
            if let Some(last) = stroke.points.last() {
                painter.circle_filled(*last, 4.0, Color32::RED);
            }
        }
    }

    /// Send closed floor plan to processing engine
    fn process_closed_floor_plan(app: &mut BerApp, stroke_idx: usize) {
        if let Some(stroke) = app.sketch_state.strokes.get(stroke_idx) {
            // Convert points to a format your engine expects
            let points: Vec<(f32, f32)> = stroke.points.iter().map(|p| (p.x, p.y)).collect();

            println!("Processing floor plan with {} points", points.len());

            // Process through the engine
            let processed = app.app_controller.process_floor_plan(points);
            println!(
                "Simplified from {} to {} points",
                processed.original_points.len(),
                processed.simplified_points.len()
            );

            // Get stats
            let stats = app
                .app_controller
                .analyze_floor_plan(&processed.simplified_points);
            println!("Floor plan stats:");
            println!("  Area: {:.1} sq units", stats.area);
            println!("  Perimeter: {:.1} units", stats.perimeter);
            println!("  Vertices: {}", stats.num_vertices);

            // Update the stroke with simplified points
            if let Some(stroke) = app.sketch_state.strokes.get_mut(stroke_idx) {
                stroke.points = processed
                    .simplified_points
                    .iter()
                    .map(|(x, y)| egui::pos2(*x, *y))
                    .collect();

                println!("Floor plan processed and updated!");
            }
        }
    }
}
