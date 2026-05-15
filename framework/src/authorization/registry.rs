use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::RwLock;

type GateFn = Box<dyn Fn(&dyn Any, &dyn Any) -> bool + Send + Sync>;

pub(crate) struct GateRegistry {
    gates: RwLock<HashMap<(String, TypeId, TypeId), GateFn>>,
}

impl GateRegistry {
    pub(crate) fn new() -> Self {
        Self {
            gates: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn register<U: 'static, R: 'static>(
        &self,
        action: &str,
        f: impl Fn(&U, &R) -> bool + Send + Sync + 'static,
    ) {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        let erased: GateFn = Box::new(move |u, r| {
            let u = u.downcast_ref::<U>().expect("gate user type");
            let r = r.downcast_ref::<R>().expect("gate resource type");
            f(u, r)
        });
        self.gates.write().unwrap().insert(key, erased);
    }

    pub(crate) fn invoke<U: 'static, R: 'static>(
        &self,
        action: &str,
        user: &U,
        resource: &R,
    ) -> Option<bool> {
        let key = (action.to_string(), TypeId::of::<U>(), TypeId::of::<R>());
        let gates = self.gates.read().unwrap();
        gates.get(&key).map(|f| f(user as &dyn Any, resource as &dyn Any))
    }
}

pub(crate) fn global() -> &'static GateRegistry {
    static R: std::sync::OnceLock<GateRegistry> = std::sync::OnceLock::new();
    R.get_or_init(GateRegistry::new)
}
