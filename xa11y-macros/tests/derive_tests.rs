use pyo3::prelude::*;
use xa11y_macros::PyBindable;

// ── String enum ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PyBindable)]
#[pyclass(frozen, eq, hash)]
enum Color {
    Red,
    DarkBlue,
    #[py_bind(string = "lime")]
    Green,
    #[py_bind(skip)]
    _Internal,
}

#[test]
fn enum_to_str() {
    assert_eq!(Color::Red.to_str(), "red");
    assert_eq!(Color::DarkBlue.to_str(), "dark_blue");
    assert_eq!(Color::Green.to_str(), "lime");
}

#[test]
fn enum_from_str_python() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let cls = py.get_type::<Color>();
        let red: Color = cls
            .call_method1("from_str", ("red",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(red, Color::Red);

        let green: Color = cls
            .call_method1("from_str", ("lime",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(green, Color::Green);

        // Unknown value should raise ValueError
        let result = cls.call_method1("from_str", ("purple",));
        assert!(result.is_err());
    });
}

#[test]
fn enum_repr_python() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let obj = Color::Red.into_pyobject(py).unwrap().into_any();
        let repr: String = obj.call_method0("__repr__").unwrap().extract().unwrap();
        assert_eq!(repr, "Color.red");

        let obj = Color::DarkBlue.into_pyobject(py).unwrap().into_any();
        let repr: String = obj.call_method0("__repr__").unwrap().extract().unwrap();
        assert_eq!(repr, "Color.dark_blue");
    });
}

// ── Class attrs enum ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PyBindable)]
#[py_bind(class_attrs)]
enum Signal {
    Start,
    StopNow,
}

#[test]
fn class_attrs_to_py_str() {
    assert_eq!(Signal::Start.to_py_str(), "start");
    assert_eq!(Signal::StopNow.to_py_str(), "stop_now");
}

#[test]
fn class_attrs_python_constants() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let cls = py.get_type::<PySignal>();
        let start: String = cls.getattr("START").unwrap().extract().unwrap();
        assert_eq!(start, "start");

        let stop: String = cls.getattr("STOP_NOW").unwrap().extract().unwrap();
        assert_eq!(stop, "stop_now");
    });
}

// ── Frozen struct ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, PyBindable)]
#[pyclass(frozen)]
struct Point {
    x: i32,
    y: i32,
    #[py_bind(skip)]
    _internal: u8,
}

#[test]
fn struct_python_getters_and_repr() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let point = Point {
            x: 10,
            y: 20,
            _internal: 0,
        };
        let obj = point.into_pyobject(py).unwrap().into_any();

        let x: i32 = obj.getattr("x").unwrap().extract().unwrap();
        assert_eq!(x, 10);

        let y: i32 = obj.getattr("y").unwrap().extract().unwrap();
        assert_eq!(y, 20);

        let repr: String = obj.call_method0("__repr__").unwrap().extract().unwrap();
        assert_eq!(repr, "Point(x=10, y=20)");
    });
}

#[test]
fn struct_python_constructor() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let cls = py.get_type::<Point>();
        let obj = cls.call1((5, 15)).unwrap();
        let x: i32 = obj.getattr("x").unwrap().extract().unwrap();
        let y: i32 = obj.getattr("y").unwrap().extract().unwrap();
        assert_eq!(x, 5);
        assert_eq!(y, 15);
    });
}
