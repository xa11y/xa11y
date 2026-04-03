use pyo3::prelude::*;
use xa11y_macros::PyBindable;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PyBindable)]
#[pyclass(frozen, eq, hash)]
enum Color {
    Red,
    DarkBlue,
}

#[test]
fn enum_to_str() {
    assert_eq!(Color::Red.to_str(), "red");
    assert_eq!(Color::DarkBlue.to_str(), "dark_blue");
}

#[test]
fn enum_from_str_and_repr() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let cls = py.get_type::<Color>();
        let red: Color = cls
            .call_method1("from_str", ("red",))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(red, Color::Red);
        assert!(cls.call_method1("from_str", ("nope",)).is_err());

        let repr: String = Color::DarkBlue
            .into_pyobject(py)
            .unwrap()
            .into_any()
            .call_method0("__repr__")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(repr, "Color.dark_blue");
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PyBindable)]
#[py_bind(class_attrs)]
enum Signal {
    Start,
    StopNow,
}

#[test]
fn class_attrs() {
    assert_eq!(Signal::Start.to_py_str(), "start");
    assert_eq!(Signal::StopNow.to_py_str(), "stop_now");
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let cls = py.get_type::<PySignal>();
        assert_eq!(
            cls.getattr("START").unwrap().extract::<String>().unwrap(),
            "start"
        );
        assert_eq!(
            cls.getattr("STOP_NOW")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "stop_now"
        );
    });
}

#[derive(Debug, Clone, Copy, PartialEq, PyBindable)]
#[pyclass(frozen)]
struct Point {
    x: i32,
    y: i32,
}

#[test]
fn struct_getters_repr_new() {
    pyo3::prepare_freethreaded_python();
    Python::with_gil(|py| {
        let obj = Point { x: 10, y: 20 }.into_pyobject(py).unwrap().into_any();
        assert_eq!(obj.getattr("x").unwrap().extract::<i32>().unwrap(), 10);
        assert_eq!(obj.getattr("y").unwrap().extract::<i32>().unwrap(), 20);
        assert_eq!(
            obj.call_method0("__repr__")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "Point(x=10, y=20)"
        );

        let cls = py.get_type::<Point>();
        let obj2 = cls.call1((5, 15)).unwrap();
        assert_eq!(obj2.getattr("x").unwrap().extract::<i32>().unwrap(), 5);
    });
}
