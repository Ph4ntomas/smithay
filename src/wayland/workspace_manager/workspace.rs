//! Workspace

use std::{
    hash::Hash,
    sync::{Arc, Mutex},
};

use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::ExtWorkspaceGroupHandleV1,
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1, State, WorkspaceCapabilities},
    ext_workspace_manager_v1::ExtWorkspaceManagerV1,
};
use wayland_server::{
    Client, Dispatch, DisplayHandle, Resource, Weak,
    backend::{ClientId, ObjectId},
};

use crate::{
    utils::user_data::UserDataMap,
    wayland::{
        Dispatch2,
        workspace_manager::{
            PendingRequest, WorkspaceHandler, WorkspaceManagerObjectData, WorkspaceManagerState,
            group::{WeakWorkspaceGroup, WorkspaceGroup},
        },
    },
};

const WORKSPACE_VERSION: u32 = 1;

/// Handle to a Workspace.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub(crate) inner: Arc<(Mutex<Inner>, UserDataMap)>,
}

/// Weak version of [`Workspace`].
#[derive(Debug, Clone)]
pub struct WeakWorkspace {
    inner: std::sync::Weak<(Mutex<Inner>, UserDataMap)>,
}

/// Workspace state
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    pub name: String,
    pub coordinates: Vec<u32>,
    pub state: State,
    pub capabilities: WorkspaceCapabilities,
}

/// Workspace internal data.
#[derive(Debug)]
pub(crate) struct Inner {
    pending_state: WorkspaceState,
    current_state: WorkspaceState,

    pending_id: Option<String>,
    current_id: Option<String>,

    pub(crate) pending_group: Option<WeakWorkspaceGroup>,
    pub(crate) group: Option<WeakWorkspaceGroup>,

    pending_removal: bool,
    pub(crate) instances: Vec<ExtWorkspaceHandleV1>,
}

/// Data associated with workspace handles.
#[derive(Debug)]
pub struct WorkspaceData {
    pub(crate) handle: WeakWorkspace,
    pub(crate) manager: Weak<ExtWorkspaceManagerV1>,
}

/// Workspace Builder
#[derive(Debug)]
pub struct Builder {
    state: WorkspaceState,
    id: Option<String>,
}

#[derive(Debug)]
pub(crate) enum PendingWorkspaceRequest {
    Activate,
    Deactivate,
    Assign(Weak<ExtWorkspaceGroupHandleV1>),
    Remove,
}

impl Workspace {
    /// Gets a builder object for [`Workspace`].
    pub fn builder(name: String) -> Builder {
        Builder {
            state: WorkspaceState {
                name,
                ..Default::default()
            },
            id: None,
        }
    }

    /// Gets the `Workspace` associated with an [`ExtWorkspaceHandleV1`].
    pub fn from_resource(resource: &ExtWorkspaceHandleV1) -> Option<Self> {
        resource.data::<WorkspaceData>().and_then(|d| d.handle.upgrade())
    }

    /// Creates a new [`WeakWorkspace`] pointing to the same Workspace.
    pub fn downgrade(&self) -> WeakWorkspace {
        WeakWorkspace {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Access the [`UserDataMap`] associated with this [`Workspace`]
    pub fn user_data(&self) -> &UserDataMap {
        &self.inner.1
    }

    /// Allow the pending state of this Workspace to be manipulated.
    pub fn with_pending_state<F, T>(&self, with_state: F) -> T
    where
        F: FnOnce(&mut WorkspaceState) -> T,
    {
        let mut guard = self.inner.0.lock().unwrap();
        with_state(&mut guard.pending_state)
    }

    /// Allow access to the current state, as seen by a client.
    pub fn with_current_state<F, T>(&self, with_state: F) -> T
    where
        F: FnOnce(&WorkspaceState) -> T,
    {
        let guard = self.inner.0.lock().unwrap();
        with_state(&guard.pending_state)
    }

    /// Check if the workspace has pending changes that have not yet been sent to clients.
    pub fn has_pending_changes(&self) -> bool {
        let guard = self.inner.0.lock().unwrap();

        guard.pending_state != guard.current_state
            || guard.pending_id.is_some()
            || guard.pending_group != guard.group
            || guard.pending_removal
    }

    /// Check if the workspace was marked for removal.
    pub fn is_pending_removal(&self) -> bool {
        self.inner.0.lock().unwrap().pending_removal
    }

    /// Sets the `Workspace`'s id.
    ///
    /// `Workspace` can be assigned a unique, stable identifier. If the id was set, this function
    /// returns `true`. If the id was previously assigned and has already been sent to clients, this
    /// function will return `false`.
    ///
    /// NOTE: It's up to the compositor to ensure the id are unique. Not doing that is a protocol
    /// violation.
    pub fn set_id(&mut self, id: String) -> bool {
        self.inner.0.lock().unwrap().set_id(id)
    }

    /// Gets the `Workspace`'s id.
    pub fn id(&mut self) -> Option<String> {
        self.inner.0.lock().unwrap().current_id.clone()
    }

    /// Assign the workspace to a group.
    pub fn assign(&self, group: WorkspaceGroup) {
        self.inner.0.lock().unwrap().pending_group = Some(group.downgrade());
    }

    /// Remove the workspace from its current group.
    pub fn unassign(&self) {
        self.inner.0.lock().unwrap().pending_group.take();
    }

    /// Access the workspace pending_group.
    pub fn pending_group(&self) -> Option<WorkspaceGroup> {
        self.inner.0
            .lock()
            .unwrap()
            .pending_group
            .as_ref()
            .and_then(WeakWorkspaceGroup::upgrade)
    }

    /// Gets the `Workspace`'s group, if any.
    pub fn group(&self) -> Option<WorkspaceGroup> {
        self.inner
            .0
            .lock()
            .unwrap()
            .group
            .as_ref()
            .and_then(WeakWorkspaceGroup::upgrade)
    }

    pub(super) fn new_instance<D>(
        &self,
        client: &Client,
        display: &DisplayHandle,
        manager: &ExtWorkspaceManagerV1,
    ) where
        D: Dispatch<ExtWorkspaceHandleV1, WorkspaceData>,
        D: 'static,
    {
        let Ok(instance) = client.create_resource::<ExtWorkspaceHandleV1, _, D>(
            display,
            WORKSPACE_VERSION,
            WorkspaceData {
                handle: self.downgrade(),
                manager: manager.downgrade(),
            },
        ) else {
            return;
        };

        let mut inner = self.inner.0.lock().unwrap();

        if !inner.current_state.name.is_empty() {
            let state = &inner.current_state;

            instance.name(state.name.clone());
            if let Some(id) = inner.current_id.clone() {
                instance.id(id);
            }

            instance.state(state.state);
            instance.capabilities(state.capabilities);

            if !state.coordinates.is_empty() {
                let coord: Vec<u8> = state
                    .coordinates
                    .iter()
                    .copied()
                    .flat_map(u32::to_ne_bytes)
                    .collect();

                instance.coordinates(coord);
            }

            if let Some(group) = inner.group.as_ref().and_then(WeakWorkspaceGroup::upgrade) {
                todo!();
            }
        }

        inner.instances.push(instance);
    }

    pub(super) fn remove_instance(&self, manager: &ExtWorkspaceManagerV1, with_events: bool) {
        let mut inner = self.inner.0.lock().unwrap();

        let manager_id = Resource::id(manager);
        let extracted = inner.instances.extract_if(.., |i| {
            Resource::id(i).same_client_as(&manager_id)
                && i.data::<WorkspaceData>().unwrap().manager.id() == manager_id
        });

        if with_events {
            for instance in extracted {
                instance.removed();
            }
        } else {
            extracted.for_each(std::mem::drop);
        }
    }
}

impl WeakWorkspace {
    /// Attempts to upgrade the `WeakWorkspace` to a [`Workspace`]
    pub fn upgrade(&self) -> Option<Workspace> {
        self.inner.upgrade().map(|inner| Workspace { inner })
    }
}

impl Inner {
    pub(crate) fn instance_with_manager(&self, manager: &ObjectId) -> Option<&ExtWorkspaceHandleV1> {
        self.instances.iter().find(|i| {
            Resource::id(*i).same_client_as(manager)
                && &i.data::<WorkspaceData>().unwrap().manager.id() == manager
        })
    }

    fn set_id(&mut self, id: String) -> bool {
        if self.current_id.is_none() {
            self.pending_id = Some(id);
            true
        } else {
            false
        }
    }

    fn apply_changes(&mut self) -> bool {
        let name_changed = self.pending_state.name != self.current_state.name;
        let coordinate_changed = self.pending_state.coordinates != self.current_state.coordinates;
        let state_changed = self.pending_state.state != self.current_state.state;
        let capabilities_changed = self.pending_state.capabilities != self.current_state.capabilities;
        let group_changed = self.pending_group != self.group;

        let changed = name_changed
            | coordinate_changed
            | state_changed
            | capabilities_changed;

        if changed {
            self.current_state = self.pending_state.clone();
        }

        let id_changed = if let Some(id) = self.pending_id.take() {
            self.current_id = Some(id);
            true
        } else {
            false
        };

        let (old_group, new_group) = if group_changed {
            let old_group = self.group.as_ref().and_then(WeakWorkspaceGroup::upgrade);
            self.group = self.pending_group.clone();

            (old_group, self.group.as_ref().and_then(WeakWorkspaceGroup::upgrade))
        } else {
            (None, None)
        };

        let changed = changed | id_changed | group_changed;

        if changed {
            let state = &self.current_state;
            let new_coordinates = coordinate_changed.then(|| {
                state.coordinates
                    .iter()
                    .copied()
                    .flat_map(u32::to_ne_bytes)
                    .collect()
            });

            let mut old_group = old_group.as_ref().map(|g| g.inner.0.lock().unwrap());
            let mut new_group = new_group.as_ref().map(|g| g.inner.0.lock().unwrap());

            for instance in &self.instances {
                if name_changed { instance.name(state.name.clone()); }
                if let Some(new_coord) = new_coordinates.clone() {
                    instance.coordinates(new_coord);
                }
                if state_changed { instance.state(state.state); }
                if capabilities_changed { instance.capabilities(state.capabilities); }
                if id_changed {
                    instance.id(self.current_id.clone().unwrap());
                }

                if let Some(old_group) = old_group.as_mut() {
                    old_group.workspace_leave(&instance);
                }

                if let Some(new_group) = new_group.as_mut() {
                    new_group.workspace_leave(&instance);
                }
            }
        }

        changed
    }
}

impl WorkspaceManagerState {
    /// Creates a new workspace with default state.
    pub fn new_workspace<D>(&mut self, name: String) -> Workspace
    where
        D: Dispatch<ExtWorkspaceHandleV1, WorkspaceData>,
        D: 'static,
    {
        self.new_workspace_with_builder::<D>(Workspace::builder(name))
    }

    /// Creates a new workspace with default state.
    pub fn new_workspace_with_builder<D>(&mut self, builder: Builder) -> Workspace
    where
        D: Dispatch<ExtWorkspaceHandleV1, WorkspaceData>,
        D: 'static,
    {
        let handle = builder.build(self.instances.len());

        for instance in &self.instances {
            let Ok(client) = self.display.get_client(instance.id()) else {
                continue;
            };

            handle.new_instance::<D>(&client, &self.display, instance);
        }

        self.workspaces.push(handle.clone());
        handle
    }

    /// Removes a [`Workspace`].
    ///
    /// This function marks the workspace for removal. The actual removal will take place when
    /// [`WorkspaceManagerState::done()`] will be called.
    ///
    /// This function remove the `Workspace` from the manager, then send workspace_leave event on its active
    /// [`WorkspaceGroup`] if any. The instance list is then drained to avoid further requests, and
    /// each instance is sent the `removed` event to notify the client.
    ///
    /// NOTE: It is the responsibility of the compositor to ensure there are no other instance of
    /// this specific [`Workspace`] to avoid the internal state and user_data leaking. However,
    /// after calling this function, the [`Workspace`] object will be functionally inert (it has no
    /// client instances, and none can be added back).
    pub fn remove_workspace(&mut self, workspace: Workspace) {
        workspace.inner.0.lock().unwrap().pending_removal = true;
    }

    /// Gets an iterator over managed [`Workspace`]
    pub fn workspaces(&self) -> std::slice::Iter<'_, Workspace> {
        self.workspaces.iter()
    }

    /// Send all pending workspace changes.
    ///
    /// This function should be called after [`WorkspaceManager::send_groups_changes`]
    pub fn send_workspaces_changes(&mut self) -> bool {
        let mut changed = false;

        self.workspaces.retain_mut(|w| {
            let mut inner = w.inner.0.lock().unwrap();

            if inner.pending_removal {
                changed = true;

                if let Some(group) = inner.group.take().and_then(|g| g.upgrade()) {
                    //TODO: remove group.
                }

                for instance in inner.instances.drain(..) {
                    instance.removed();
                }

                return false;
            }

            changed |= inner.apply_changes();
            true
        });

        changed
    }

}

impl<D> Dispatch2<ExtWorkspaceHandleV1, D> for WorkspaceData
where
    D: WorkspaceHandler,
{
    fn request(
        &self,
        state: &mut D,
        client: &wayland_server::Client,
        resource: &ExtWorkspaceHandleV1,
        request: <ExtWorkspaceHandleV1 as Resource>::Request,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        let Some(handle) = self.handle.upgrade() else {
            return;
        };

        let Some(manager) = self.manager.upgrade().ok() else {
            return;
        };

        let capabilities = handle.inner.0.lock().unwrap().current_state.capabilities;

        use ext_workspace_handle_v1::Request;
        let pending = match request {
            Request::Destroy => {
                self.destroyed(state, client.id(), resource);
                return;
            }
            Request::Activate if capabilities.contains(WorkspaceCapabilities::Activate) => {
                WorkspaceHandler::activate(state, resource);
                PendingWorkspaceRequest::Activate
            }
            Request::Deactivate if capabilities.contains(WorkspaceCapabilities::Deactivate) => {
                WorkspaceHandler::deactivate(state, resource);
                PendingWorkspaceRequest::Deactivate
            }
            Request::Assign { workspace_group } if capabilities.contains(WorkspaceCapabilities::Assign) => {
                WorkspaceHandler::assign(state, resource, &workspace_group);
                PendingWorkspaceRequest::Assign(workspace_group.downgrade())
            }
            Request::Remove if capabilities.contains(WorkspaceCapabilities::Remove) => {
                WorkspaceHandler::remove(state, resource);
                PendingWorkspaceRequest::Remove
            }
            _ => {
                return;
            }
        };

        WorkspaceManagerObjectData::add_pending(
            &manager,
            PendingRequest::Workspace(resource.downgrade(), pending),
        );
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &ExtWorkspaceHandleV1) {
        let Some(handle) = self.handle.upgrade() else {
            return;
        };

        let mut workspace = handle.inner.0.lock().unwrap();

        if let Ok(manager) = self.manager.upgrade() {
            WorkspaceManagerObjectData::remove_pending(&manager, Resource::id(resource));
        }

        if let Some(pos) = workspace
            .instances
            .iter()
            .position(|w| Resource::id(w) == Resource::id(resource))
        {
            let instance = workspace.instances.remove(pos);
            WorkspaceHandler::workspace_destroyed(state, &instance);
        }
    }
}

impl Builder {
    /// Sets the id to use on [`Workspace`] creation.
    ///
    /// NOTE: Workspace id are supposed to be unique. It is the responsibility of the compositor to
    /// ensure this holds true at all time.
    pub fn with_id(self, id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            ..self
        }
    }

    /// Sets the coordinates to use on [`Workspace`] creation.
    pub fn with_coordinates(self, coordinates: impl Into<Vec<u32>>) -> Self {
        Self {
            state: WorkspaceState {
                coordinates: coordinates.into(),
                ..self.state
            },
            ..self
        }
    }

    /// Sets the [`Workspace`] initial state.
    pub fn with_state(self, state: State) -> Self {
        Self {
            state: WorkspaceState {
                state,
                ..self.state
            },
            ..self
        }
    }

    /// Sets the [`Workspace`] initial capabilities.
    pub fn with_capabilities(self, capabilities: WorkspaceCapabilities) -> Self {
        Self {
            state: WorkspaceState {
                capabilities,
                ..self.state
            },
            ..self
        }
    }

    fn build(self, capacity: usize) -> Workspace {
        let Self { state, id } = self;
        let inner = Arc::new((
            Mutex::new(Inner {
                pending_state: state,
                current_state: Default::default(),
                pending_id: id,
                current_id: None,

                pending_removal: false,

                pending_group: None,
                group: None,
                instances: Vec::with_capacity(capacity),
            }),
            UserDataMap::default(),
        ));

        Workspace { inner }
    }
}

impl PartialEq for Workspace {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for Workspace {}

impl Hash for Workspace {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

impl PartialEq for WeakWorkspace {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        std::sync::Weak::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for WeakWorkspace {}

impl Hash for WeakWorkspace {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Weak::as_ptr(&self.inner).hash(state);
    }
}

impl PartialEq<WeakWorkspace> for Workspace {
    #[inline]
    fn eq(&self, other: &WeakWorkspace) -> bool {
        other.upgrade().map(|o| &o == self).unwrap_or(false)
    }
}

impl PartialEq<Workspace> for WeakWorkspace {
    #[inline]
    fn eq(&self, other: &Workspace) -> bool {
        self.upgrade().map(|o| &o == other).unwrap_or(false)
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            name: String::new(),
            coordinates: Vec::new(),
            state: State::empty(),
            capabilities: WorkspaceCapabilities::all(),
        }
    }
}
