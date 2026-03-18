use anyhow::{anyhow, Result};
use gtk::glib::{self, object_subclass, wrapper, Properties};
use gtk::{cairo, gdk, prelude::*, subclass::prelude::*};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, LazyLock};
use std::time::{Duration, Instant};

use crate::error_handling_ctx;

// --- GLOBAL STORAGE ---

static GRAPH_REGISTRY: LazyLock<Mutex<HashMap<String, Arc<Mutex<GraphSharedState>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct GraphSharedState {
    pub history: VecDeque<(Instant, f64)>,
    pub extra_point: Option<(Instant, f64)>,
    pub last_updated_at: Instant,
    pub time_range_ms: u64,
}

impl Default for GraphSharedState {
    fn default() -> Self {
        Self {
            history: VecDeque::new(),
            extra_point: None,
            last_updated_at: Instant::now(),
            time_range_ms: 10000,
        }
    }
}

pub fn push_graph_data(name: &str, val: f64) {
    if name.is_empty() { return; }

    let state_arc = {
        let mut registry = GRAPH_REGISTRY.lock().unwrap();
        registry
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(GraphSharedState::default())))
            .clone()
    };

    if let Ok(mut lock) = state_arc.lock() {
        let now = Instant::now();
        lock.last_updated_at = now;
        let tr = lock.time_range_ms;

        while let Some(entry) = lock.history.front() {
            if now.duration_since(entry.0).as_millis() as u64 > tr {
                lock.extra_point = lock.history.pop_front();
            } else {
                break;
            }
        }
        lock.history.push_back((now, val));
    }; 
}

// --- WIDGET ---

wrapper! {
    pub struct Graph(ObjectSubclass<GraphPriv>)
    @extends gtk::Bin, gtk::Container, gtk::Widget;
}

#[derive(Properties)]
#[properties(wrapper_type = Graph)]
pub struct GraphPriv {
    #[property(get, set, nick = "Name", default = "")]
    name: RefCell<String>,

    // Re-added this so GTK/Eww doesn't panic
    #[property(get, set, nick = "Value", default = 0f64)]
    value: RefCell<f64>,

    #[property(get, set, nick = "Thickness", default = 1f64)]
    thickness: RefCell<f64>,
    #[property(get, set, nick = "Line Style", default = "miter")]
    line_style: RefCell<String>,
    #[property(get, set, nick = "Min", default = 0f64)]
    min: RefCell<f64>,
    #[property(get, set, nick = "Max", default = 100f64)]
    max: RefCell<f64>,
    #[property(get, set, nick = "Dynamic", default = true)]
    dynamic: RefCell<bool>,
    #[property(get, set, nick = "Time Range", default = 10000u64)]
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
            shared_state: RefCell::new(Arc::new(Mutex::new(GraphSharedState::default()))),
            tick_source: RefCell::new(None),
        }
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
                    let state = {
                        let mut registry = GRAPH_REGISTRY.lock().unwrap();
                        registry
                            .entry(name.clone())
                            .or_insert_with(|| Arc::new(Mutex::new(GraphSharedState::default())))
                            .clone()
                    };
                    self.shared_state.replace(state);
                }
                self.name.replace(name);
            }
            "value" => {
                let val: f64 = value.get().unwrap();
                self.value.replace(val); // Keep the local property in sync
                push_graph_data(&self.name.borrow(), val);
            }
            "time-range" => {
                let tr: u64 = value.get().unwrap();
                self.time_range.replace(tr);
                if let Ok(mut lock) = self.shared_state.borrow().lock() {
                    lock.time_range_ms = tr;
                };
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

impl ContainerImpl for GraphPriv {}
impl BinImpl for GraphPriv {}

impl WidgetImpl for GraphPriv {
    fn map(&self) {
        self.parent_map();
        
        // Ensure we are looking at the correct registry state
        let name = self.name.borrow().clone();
        if !name.is_empty() {
            let registry = GRAPH_REGISTRY.lock().unwrap();
            if let Some(state) = registry.get(&name) {
                self.shared_state.replace(state.clone());
            }
        }

        let obj_weak = self.obj().downgrade();
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
            
            if lock.history.is_empty() { return Ok(()); }

            let (min, max) = {
                let mut max = *self.max.borrow();
                let min = *self.min.borrow();
                if *self.dynamic.borrow() {
                    for (_, value) in &lock.history {
                        if *value > max { max = *value; }
                    }
                }
                (min, max)
            };

            let styles = self.obj().style_context();
            let margin = styles.margin(gtk::StateFlags::NORMAL);
            let width = self.obj().allocated_width() as f64 - (margin.left + margin.right) as f64;
            let height = self.obj().allocated_height() as f64 - (margin.top + margin.bottom) as f64;

            let points = {
                let value_range = if (max - min).abs() < 1e-7 { 1.0 } else { max - min };
                let time_range = *self.time_range.borrow() as f64;
                let now = Instant::now();
                
                lock.history.iter().map(|(instant, value)| {
                    let t = now.duration_since(*instant).as_millis() as f64;
                    let x_ratio = t / time_range;
                    let y_ratio = (value - min) / value_range;

                    let x = if *self.flip_x.borrow() { 1.0 - x_ratio } else { x_ratio };
                    let y = if *self.flip_y.borrow() { 1.0 - y_ratio } else { y_ratio };
                    let (px, py) = if *self.vertical.borrow() { (y, x) } else { (x, y) };
                    (width * px, height * py)
                }).collect::<Vec<(f64, f64)>>()
            };

            cr.save()?;
            cr.translate(margin.left as f64, margin.top as f64);
            cr.rectangle(0.0, 0.0, width, height);
            cr.clip();

            if !points.is_empty() {
                let color: gdk::RGBA = styles.color(gtk::StateFlags::NORMAL);
                cr.set_source_rgba(color.red(), color.green(), color.blue(), color.alpha());
                cr.set_line_width(*self.thickness.borrow());
                
                let mut iter = points.iter();
                if let Some((x, y)) = iter.next() {
                    cr.move_to(*x, *y);
                    for (x, y) in iter { cr.line_to(*x, *y); }
                }
                apply_line_style(&self.line_style.borrow(), cr)?;
                cr.stroke()?;
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
        _ => return Err(anyhow!("Invalid line style")),
    }
    Ok(())
}

impl Graph {
    pub fn new() -> Self { glib::Object::new::<Self>() }
}