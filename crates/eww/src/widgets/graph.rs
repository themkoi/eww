use anyhow::{anyhow, Result};
use gtk::glib::{self, object_subclass, wrapper, Properties};
use gtk::{cairo, gdk, prelude::*, subclass::prelude::*};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, LazyLock};
use std::time::{Duration, Instant};

use crate::error_handling_ctx;

// Global storage to persist data across widget re-creation
static GRAPH_REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<GraphSharedState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The actual data that persists
// Remove #[derive(Default)] here
struct GraphSharedState {
    history: VecDeque<(Instant, f64)>,
    extra_point: Option<(Instant, f64)>,
    last_updated_at: Instant,
}

// Manually implement Default to provide Instant::now()
impl Default for GraphSharedState {
    fn default() -> Self {
        Self {
            history: VecDeque::new(),
            extra_point: None,
            last_updated_at: Instant::now(),
        }
    }
}

wrapper! {
    pub struct Graph(ObjectSubclass<GraphPriv>)
    @extends gtk::Bin, gtk::Container, gtk::Widget;
}

#[derive(Properties)]
#[properties(wrapper_type = Graph)]
pub struct GraphPriv {
    #[property(get, set, nick = "Name", blurb = "Unique ID for data persistence", default = "")]
    name: RefCell<String>,

    #[property(get, set, nick = "Value", minimum = 0f64, maximum = f64::MAX, default = 0f64)]
    value: RefCell<f64>,

    #[property(get, set, nick = "Thickness", minimum = 0f64, maximum = f64::MAX, default = 1f64)]
    thickness: RefCell<f64>,

    #[property(get, set, nick = "Line Style", default = "miter")]
    line_style: RefCell<String>,

    #[property(get, set, nick = "Min", minimum = 0f64, maximum = f64::MAX, default = 0f64)]
    min: RefCell<f64>,

    #[property(get, set, nick = "Max", minimum = 0f64, maximum = f64::MAX, default = 100f64)]
    max: RefCell<f64>,

    #[property(get, set, nick = "Dynamic", default = true)]
    dynamic: RefCell<bool>,

    #[property(get, set, nick = "Time Range", minimum = 0u64, maximum = u64::MAX, default = 10000u64)]
    time_range: RefCell<u64>,

    #[property(get, set, nick = "Flip X", default = true)]
    flip_x: RefCell<bool>,
    #[property(get, set, nick = "Flip Y", default = true)]
    flip_y: RefCell<bool>,
    #[property(get, set, nick = "Vertical", default = false)]
    vertical: RefCell<bool>,

    shared_state: RefCell<Arc<Mutex<GraphSharedState>>>,
    tick_source: RefCell<Option<glib::SourceId>>,
}

impl Default for GraphPriv {
    fn default() -> Self {
        Self {
            name: RefCell::new(String::new()),
            value: RefCell::new(0.0),
            thickness: RefCell::new(1.0),
            line_style: RefCell::new("miter".to_string()),
            min: RefCell::new(0.0),
            max: RefCell::new(100.0),
            dynamic: RefCell::new(true),
            time_range: RefCell::new(10000),
            flip_x: RefCell::new(true),
            flip_y: RefCell::new(true),
            vertical: RefCell::new(false),
            shared_state: RefCell::new(Arc::new(Mutex::new(GraphSharedState {
                last_updated_at: Instant::now(),
                ..Default::default()
            }))),
            tick_source: RefCell::new(None),
        }
    }
}

impl GraphPriv {
    fn value_to_point(&self, width: f64, height: f64, x: f64, y: f64) -> (f64, f64) {
        let x = if *self.flip_x.borrow() { 1.0 - x } else { x };
        let y = if *self.flip_y.borrow() { 1.0 - y } else { y };
        let (x, y) = if *self.vertical.borrow() { (y, x) } else { (x, y) };
        (width * x, height * y)
    }
}

impl ObjectImpl for GraphPriv {
    fn properties() -> &'static [glib::ParamSpec] {
        Self::derived_properties()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "name" => {
                let name: String = value.get().unwrap();
                if !name.is_empty() {
                    let mut registry = GRAPH_REGISTRY.lock().unwrap();
                    let state = registry.entry(name.clone()).or_insert_with(|| {
                        Arc::new(Mutex::new(GraphSharedState {
                            last_updated_at: Instant::now(),
                            ..Default::default()
                        }))
                    });
                    self.shared_state.replace(Arc::clone(state));
                }
                self.name.replace(name);
            }
            "value" => {
                let val: f64 = value.get().unwrap();
                self.value.replace(val);
                
                let state = self.shared_state.borrow();
                if let Ok(mut lock) = state.lock() {
                    let now = Instant::now();
                    lock.last_updated_at = now;
                    let tr = *self.time_range.borrow();
                    
                    while let Some(entry) = lock.history.front() {
                        if now.duration_since(entry.0).as_millis() as u64 > tr {
                            lock.extra_point = lock.history.pop_front();
                        } else {
                            break;
                        }
                    }
                    lock.history.push_back((now, val));
                }
                self.obj().queue_draw();
            }
            _ => { self.derived_set_property(_id, value, pspec); }
        }
    }

    fn property(&self, id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        self.derived_property(id, pspec)
    }
}

#[object_subclass]
impl ObjectSubclass for GraphPriv {
    type ParentType = gtk::Bin;
    type Type = Graph;
    const NAME: &'static str = "Graph";
    fn class_init(klass: &mut Self::Class) {
        klass.set_css_name("graph");
    }
}

impl ContainerImpl for GraphPriv {
    fn add(&self, _widget: &gtk::Widget) {
        error_handling_ctx::print_error(anyhow!("Graph widget shouldn't have children"));
    }
}
impl BinImpl for GraphPriv {}

impl WidgetImpl for GraphPriv {
    fn map(&self) {
        self.parent_map();
        let obj_weak = self.obj().downgrade();
        // Start high-frequency redraw when visible
        let source_id = glib::timeout_add_local(Duration::from_millis(33), move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.queue_draw();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        self.tick_source.replace(Some(source_id));
    }

    fn unmap(&self) {
        if let Some(source_id) = self.tick_source.borrow_mut().take() {
            source_id.remove();
        }
        self.parent_unmap();
    }

    fn draw(&self, cr: &cairo::Context) -> glib::Propagation {
        let res: Result<()> = (|| {
            let state_lock = self.shared_state.borrow();
            let lock = state_lock.lock().map_err(|_| anyhow!("Lock poisoned"))?;
            
            let history = &lock.history;
            let extra_point = lock.extra_point;
            let last_updated_at = lock.last_updated_at;

            let (min, max) = {
                let mut max = *self.max.borrow();
                let min = *self.min.borrow();
                if *self.dynamic.borrow() {
                    for (_, value) in history {
                        if *value > max { max = *value; }
                    }
                    if let Some((_, value)) = extra_point {
                        if value > max { max = value; }
                    }
                }
                (min, max)
            };

            let styles = self.obj().style_context();
            let margin = styles.margin(gtk::StateFlags::NORMAL);
            let width = self.obj().allocated_width() as f64 - (margin.left + margin.right) as f64;
            let height = self.obj().allocated_height() as f64 - (margin.top + margin.bottom) as f64;

            let points = {
                let value_range = if max == min { 1.0 } else { max - min };
                let time_range = *self.time_range.borrow() as f64;
                
                let mut pts = history
                    .iter()
                    .map(|(instant, value)| {
                        let t = last_updated_at.duration_since(*instant).as_millis() as f64;
                        self.value_to_point(width, height, t / time_range, (value - min) / value_range)
                    })
                    .collect::<VecDeque<(f64, f64)>>();

                if let Some((instant, value)) = extra_point {
                    let t = last_updated_at.duration_since(instant).as_millis() as f64;
                    let (x, y) = self.value_to_point(width, height, (t - time_range) / time_range, (value - min) / value_range);
                    pts.push_front(if *self.vertical.borrow() { (x, -y) } else { (-x, y) });
                }
                pts
            };

            cr.save()?;
            cr.translate(margin.left as f64, margin.top as f64);
            cr.rectangle(0.0, 0.0, width, height);
            cr.clip();

            if !points.is_empty() {
                // Background Fill
                let bg_color: gdk::RGBA = styles.style_property_for_state("background-color", gtk::StateFlags::NORMAL).get()?;
                if bg_color.alpha() > 0.0 {
                    cr.move_to(points.front().unwrap().0, height);
                    for (x, y) in &points { cr.line_to(*x, *y); }
                    cr.line_to(points.back().unwrap().0, height);
                    cr.set_source_rgba(bg_color.red(), bg_color.green(), bg_color.blue(), bg_color.alpha());
                    cr.fill()?;
                }

                // Line Stroke
                let line_color: gdk::RGBA = styles.color(gtk::StateFlags::NORMAL);
                let thickness = *self.thickness.borrow();
                if line_color.alpha() > 0.0 && thickness > 0.0 {
                    let mut iter = points.iter();
                    if let Some((first_x, first_y)) = iter.next() {
                        cr.move_to(*first_x, *first_y);
                        for (x, y) in iter { cr.line_to(*x, *y); }
                    }
                    apply_line_style(&self.line_style.borrow(), cr)?;
                    cr.set_line_width(thickness);
                    cr.set_source_rgba(line_color.red(), line_color.green(), line_color.blue(), line_color.alpha());
                    cr.stroke()?;
                }
            }

            cr.restore()?;
            Ok(())
        })();

        if let Err(error) = res { error_handling_ctx::print_error(error) };
        glib::Propagation::Proceed
    }
}

fn apply_line_style(style: &str, cr: &cairo::Context) -> Result<()> {
    match style {
        "miter" => { cr.set_line_cap(cairo::LineCap::Butt); cr.set_line_join(cairo::LineJoin::Miter); }
        "bevel" => { cr.set_line_cap(cairo::LineCap::Square); cr.set_line_join(cairo::LineJoin::Bevel); }
        "round" => { cr.set_line_cap(cairo::LineCap::Round); cr.set_line_join(cairo::LineJoin::Round); }
        _ => return Err(anyhow!("Invalid line style: {}", style)),
    }
    Ok(())
}

impl Graph {
    pub fn new() -> Self { glib::Object::new::<Self>() }
}