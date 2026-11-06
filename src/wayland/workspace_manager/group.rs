//! Workspace Group

use std::{
    collections::HashSet, hash::Hash, ops::Deref, sync::{Arc, Mutex, atomic::Ordering},
};

use wayland_protocols::ext::workspace::v1::server::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1, GroupCapabilities}, ext_workspace_handle_v1::ExtWorkspaceHandleV1, ext_workspace_manager_v1::ExtWorkspaceManagerV1,
};
use wayland_server::{
    Client, Dispatch, DisplayHandle, Resource, Weak, backend::{ClientId, ObjectId}, protocol::wl_output::WlOutput,
};

use crate::{
    output::{Output, WeakOutput}, utils::user_data::UserDataMap, wayland::{
        Dispatch2, workspace_manager::{
            PendingRequest, WorkspaceHandler, WorkspaceManagerObjectData, WorkspaceManagerState, workspace::{self, WeakWorkspace, Workspace, WorkspaceData},
        },
    },
};

const GROUP_VERSION: u32 = 1;

/// A handle to a workspace group.
#[derive(Debug, Clone)]
pub struct WorkspaceGroup {
    pub(crate) inner: Arc<(Mutex<Inner>, UserDataMap)>,
}

/// Weak version of [`WorkspaceGroup`].
#[derive(Debug, Clone)]
pub struct WeakWorkspaceGroup {
    inner: std::sync::Weak<(Mutex<Inner>, UserDataMap)>,
}

#[derive(Debug, PartialEq, Eq)]
struct WorkspaceGroupState {
    capabilities: GroupCapabilities,
}

/// Workspace internal data.
#[derive(Debug)]
pub(crate) struct Inner {
    pending_state: WorkspaceGroupState,
    current_state: WorkspaceGroupState,

    pending_outputs: HashSet<WeakOutput>,
    outputs: HashSet<WeakOutput>,

    workspaces: Vec<WeakWorkspace>,

    newly_created: bool,
    pending_removal: bool,
    instances: Vec<ExtWorkspaceGroupHandleV1>,
}

/// Data associated with workspace group handle.
#[derive(Debug)]
pub struct WorkspaceGroupData {
    handle: WeakWorkspaceGroup,
    manager: Weak<ExtWorkspaceManagerV1>,
}

#[derive(Debug)]
pub(crate) enum PendingGroupRequest {
    CreateWorkspace(String),
}

impl WorkspaceGroup {
    fn new(capabilities: GroupCapabilities, capacity: usize) -> Self {
        let inner = Arc::new((
            Mutex::new(Inner {
                pending_state: WorkspaceGroupState { capabilities },
                current_state: WorkspaceGroupState { capabilities: GroupCapabilities::empty() },

                pending_outputs: Default::default(),
                outputs: Default::default(),

                workspaces: Default::default(),

                newly_created: true,
                pending_removal: false,
                instances: Vec::with_capacity(capacity),
            }),
            UserDataMap::default(),
        ));

        Self { inner }
    }

    /// Gets the `WorkspaceGroup` associated with an [`ExtWorkspaceGroupHandleV1`].
    pub fn from_resource(resource: &ExtWorkspaceGroupHandleV1) -> Option<Self> {
        resource
            .data::<WorkspaceGroupData>()
            .and_then(|d| d.handle.upgrade())
    }

    /// Creates a new [`WeakWorkspaceGroup`] pointing to the same workspace group.
    pub fn downgrade(&self) -> WeakWorkspaceGroup {
        WeakWorkspaceGroup {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Access the [`UserDataMap`] associated with this [`WorkspaceGroup`].
    pub fn user_data(&self) -> &UserDataMap {
        &self.inner.1
    }

    pub fn has_pending_changes(&self) -> bool {
        let guard = self.inner.0.lock().unwrap();

        guard.pending_state != guard.current_state || guard.newly_created || guard.pending_removal
    }

    ///// Gets the workspace group's capabilities.
    //pub fn capabilities(&self) -> GroupCapabilities {
        //self.inner.0.lock().unwrap().capabilities
    //}

    ///// Sets the workspace group capabilities.
    //pub fn set_capabilities(&self, capabilities: GroupCapabilities) {
        //self.inner.0.lock().unwrap().set_capabilities(capabilities);
    //}

    ///// Gets the list of [`Output`] in this group.
    //pub fn outputs(&self) -> Vec<Output> {
    //    let inner = self.inner.0.lock().unwrap();
    //    inner.outputs.iter().filter_map(|o| o.upgrade()).collect()
    //}

    ///// Check whether an [`Output`] is in this group.
    //pub fn has_output(&self, output: &Output) -> bool {
        //let inner = self.inner.0.lock().unwrap();

        //inner.outputs.iter().find(|o| o == &output).is_some()
    //}

    /// Adds an [`Output`] to this `WorkspaceGroup`.
    ///
    /// If the output is already in the workspace group, this function does nothing.
    /// Otherwise, the output is added and the `output_enter` signal is sent to all instances.
    pub fn output_enter(&self, output: &Output) {
        self.inner.0.lock().unwrap().pending_outputs.insert(output.downgrade());//.add_output(self, output);
    }

    /// Removes an [`Output`] to this `WorkspaceGroup`.
    ///
    /// If the output isn't in the workspace group, this function does nothing.
    /// Otherwise, the output is removed and the `output_leave` signal is sent to all instances.
    pub fn output_leave(&self, output: &Output) {
        self.inner.0.lock().unwrap().pending_outputs.retain(|o| o != output);
    }

    ///// Gets the workspaces currently part of this `WorkspaceGroup`.
    //pub fn workspaces(&self) -> Vec<Workspace> {
        //let inner = self.inner.0.lock().unwrap();
        //inner.workspaces.iter().filter_map(|w| w.upgrade()).collect()
    //}

    ///// Adds a [`Workspace`] to this `WorkspaceGroup`.
    /////
    ///// If the `Workspace` is already in the group, this function does nothing.
    ///// Otherwise, if the `Workspace` is already in a group, the `workspace_leave` signal will be
    ///// sent to all instances, followed by the `workspace_enter` signal.
    //pub fn workspace_enter(&self, workspace: &Workspace) {
        //let mut inner = self.inner.0.lock().unwrap();

        //if inner.pending_workspaces.iter().any(|w| w == workspace) {
            //return;
        //}

        //inner.pending_workspaces.push(workspace.downgrade());
        //workspace.inner.0.lock().unwrap().pending_group = Some(self.downgrade());
    //}

    ///// Remove a [`Workspace`] from this `WorkspaceGroup`.
    //pub fn workspace_leave(&self, workspace: &Workspace) {
        //let mut inner = self.inner.0.lock().unwrap();

        //if let Some(pos) = inner.pending_workspaces.iter().position(|w| w == workspace) {
            //inner.pending_workspaces.remove(pos);
        //}

        //self.inner.0.lock().unwrap().remove_workspace(self, workspace);
        //workspace.inner.0.lock().unwrap().pending_group = None;
    //}

    pub(super) fn new_instance<D>(
        &self,
        client: &Client,
        display: &DisplayHandle,
        manager: &ExtWorkspaceManagerV1,
    ) where
        D: Dispatch<ExtWorkspaceGroupHandleV1, WorkspaceGroupData>,
        D: 'static,
    {
        let Ok(instance) = client.create_resource::<ExtWorkspaceGroupHandleV1, _, D>(
            display,
            GROUP_VERSION,
            WorkspaceGroupData {
                handle: self.downgrade(),
                manager: manager.downgrade(),
            },
        ) else {
            return;
        };

        let mut inner = self.inner.0.lock().unwrap();

        //for workspace in &inner.workspaces {
            //let Some(workspace) = workspace.upgrade() else {
                //continue;
            //};

            //let w_inner = workspace.inner.0.lock().unwrap();
            //let Some(w_instance) = w_inner.instance_with_manager(&manager.id()) else {
                //continue;
            //};

            //instance.workspace_enter(w_instance);
        //}

        for output in &inner.outputs {
            let Some(output) = output.upgrade() else {
                continue;
            };

            for o_instance in output.client_outputs(client) {
                instance.output_enter(&o_instance);
            }
        }

        inner.instances.push(instance);
    }

    pub(super) fn remove_instance(&self, manager: &ExtWorkspaceManagerV1, with_events: bool) {
        let mut inner = self.inner.0.lock().unwrap();

        let manager_id = Resource::id(manager);
        let workspaces: Vec<Workspace> = inner.workspaces.iter().filter_map(|w| w.upgrade()).collect();

        let extracted = inner.instances.extract_if(.., |i| {
            Resource::id(i).same_client_as(&manager_id)
                && i.data::<WorkspaceGroupData>().unwrap().manager.id() == manager_id
        });

        if with_events {
            for instance in extracted {
                for workspace in workspaces.iter() {
                    let w_inner = workspace.inner.0.lock().unwrap();

                    if let Some(w_instance) = w_inner.instance_with_manager(&manager_id) {
                        instance.workspace_leave(w_instance);
                    }
                }

                instance.removed();
            }
        } else {
            extracted.for_each(std::mem::drop);
        }
    }

    pub(crate) fn on_output_bind(&self, client: &Client, output: &WlOutput) {
        self.inner.0.lock().unwrap().on_output_bind(client.id(), output);
    }
}

impl WeakWorkspaceGroup {
    /// Attempts to upgrade the `WeakWorkspaceGroup` to a [`WorkspaceGroup`]
    pub fn upgrade(&self) -> Option<WorkspaceGroup> {
        self.inner.upgrade().map(|inner| WorkspaceGroup { inner })
    }
}

impl Inner {
    fn on_output_bind(&mut self, client: ClientId, output: &WlOutput) {
        for instance in self
            .instances
            .iter()
            .filter(|i| i.client().is_some_and(|c| c.id() == client))
        {
            instance.output_enter(output);

            let Ok(manager) = instance.data::<WorkspaceGroupData>().unwrap().manager.upgrade() else {
                continue;
            };

            manager.done();
        }
    }

    pub(crate) fn instances_with_manager(&self, manager: ObjectId) -> impl Iterator<Item = ExtWorkspaceGroupHandleV1> {
        self.instances.iter().cloned().filter(|i| {
            Resource::id(i).same_client_as(&manager)
                && &i.data::<WorkspaceData>().unwrap().manager.id() == manager
        })
    }

    pub(crate) fn workspace_enter(&mut self, workspace: &ExtWorkspaceHandleV1) {
        let data = workspace.data::<WorkspaceData>().unwrap();
        let manager_id = data.manager.id();

        for instance in self.instances_with_manager(manager_id) {
            instance.workspace_enter(workspace);
        }
    }

    pub(crate) fn workspace_leave(&mut self, workspace: &ExtWorkspaceHandleV1) {
        let data = workspace.data::<WorkspaceData>().unwrap();

        let manager_id = data.manager.id();

        for instance in self.instances_with_manager(manager_id) {
            instance.workspace_leave(workspace);
        }
    }

    fn apply_changes(&mut self) -> bool {
        let caps_changed = self.pending_state.capabilities != self.current_state.capabilities;

        if caps_changed {
            self.current_state.capabilities = self.pending_state.capabilities;
        }

        let changed = caps_changed;

        let new_outputs: Vec<_> = self.pending_outputs
            .difference(&self.outputs)
            .filter_map(WeakOutput::upgrade)
            .collect();
        let old_outputs: Vec<_> = self.outputs
            .difference(&self.pending_outputs)
            .filter_map(WeakOutput::upgrade)
            .collect();

        self.outputs = self.pending_outputs.clone();

        let changed = changed | !new_outputs.is_empty() | !new_outputs.is_empty();

        if changed {
            let state = &self.current_state;

            for instance in &self.instances {
                if caps_changed { instance.capabilities(state.capabilities); };

                if let Some(client) = instance.client() {
                    old_outputs
                        .iter()
                        .flat_map(|o| o.client_outputs(&client))
                        .for_each(|wl| instance.output_leave(&wl));
                    new_outputs
                        .iter()
                        .flat_map(|o| o.client_outputs(&client))
                        .for_each(|wl| instance.output_enter(&wl));
                }
            }
        }

        changed
    }
}

impl WorkspaceManagerState {
    /// Creates a new [`WorkspaceGroup`] with defaults capabilities.
    pub fn new_group<D>(&mut self) -> WorkspaceGroup
    where
        D: Dispatch<ExtWorkspaceGroupHandleV1, WorkspaceGroupData>,
        D: 'static,
    {
        self.new_group_with_capabilities::<D>(GroupCapabilities::all())
    }

    /// Creates a new [`WorkspaceGroup`] with a given set of capabilities.
    pub fn new_group_with_capabilities<D>(&mut self, capabilities: GroupCapabilities) -> WorkspaceGroup
    where
        D: Dispatch<ExtWorkspaceGroupHandleV1, WorkspaceGroupData>,
        D: 'static,
    {
        let group = WorkspaceGroup::new(capabilities, self.instances.len());

        for instance in &self.instances {
            let Ok(client) = self.display.get_client(instance.id()) else {
                continue;
            };

            group.new_instance::<D>(&client, &self.display, instance);
        }

        self.groups.push(group.clone());
        group
    }

    /// Removes a [`WorkspaceGroup`]
    ///
    /// This function remove the `WorkspaceGroup` from the manager, then send `workspace_leave` and
    /// `output_leave` for its [`Workspace`] and [`Output`] respectively. The instance list is then
    /// drained to avoid further requests, and each instance is sent the `removed` event to notify
    /// the client.
    ///
    /// NOTE: It is the responsibility of the compositor to ensure there are no other instances of
    /// this specific [`WorkspaceGroup`] to avoid internal state and user data leaking. However,
    /// after calling this function the [`WorkspaceGroup`] will be functionally inert (it contains
    /// no client instances, and none can be added back).
    pub fn remove_group(&mut self, group: WorkspaceGroup) {
        //self.groups.retain(|g| g != &group);

        //let workspaces = group.workspaces();
        //for workspace in workspaces {
            //group.workspace_leave(&workspace);
        //}

        //let outputs = group.outputs();
        //for output in outputs {
            //group.output_leave(&output);
        //}

        //let mut inner = group.inner.0.lock().unwrap();

        //for instance in inner.instances.drain(..) {
            //instance.removed();
        //}
        //inner.done_needed.take();

        //self.done_needed.store(true, Ordering::Release);
    }

    /// Gets an iterator over managed [`WorkspaceGroup`].
    pub fn groups(&self) -> std::slice::Iter<'_, WorkspaceGroup> {
        self.groups.iter()
    }

    /// Send groups changes.
    ///
    /// This function will send groups capabilities as well as outpus enter/leave events for all
    /// groups. If called explicitely, it should be called before
    /// [`WorkspaceManagerState::send_workspaces_changes`] so groups are properly created by the
    /// time workspace_enter/leave events are sent.
    pub fn send_groups_changes(&mut self) -> bool {
        let mut changed = false;

        changed
    }
}

impl<D> Dispatch2<ExtWorkspaceGroupHandleV1, D> for WorkspaceGroupData
where
    D: WorkspaceHandler,
{
    fn request(
        &self,
        state: &mut D,
        client: &wayland_server::Client,
        resource: &ExtWorkspaceGroupHandleV1,
        request: <ExtWorkspaceGroupHandleV1 as wayland_server::Resource>::Request,
        _dhandle: &wayland_server::DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        let Some(handle) = self.handle.upgrade() else {
            return;
        };

        let Some(manager) = self.manager.upgrade().ok() else {
            return;
        };

        let capabilities = handle.capabilities();

        use ext_workspace_group_handle_v1::Request;
        let pending = match request {
            Request::CreateWorkspace { workspace }
                if capabilities.contains(GroupCapabilities::CreateWorkspace) =>
            {
                WorkspaceHandler::create_workspace(state, resource, workspace.clone());
                PendingGroupRequest::CreateWorkspace(workspace.clone())
            }
            Request::Destroy => {
                self.destroyed(state, client.id(), resource);
                return;
            }
            _ => {
                return;
            }
        };

        WorkspaceManagerObjectData::add_pending(
            &manager,
            PendingRequest::Group(resource.downgrade(), pending),
        );
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, resource: &ExtWorkspaceGroupHandleV1) {
        let Some(handle) = self.handle.upgrade() else {
            return;
        };

        let mut group = handle.inner.0.lock().unwrap();

        if let Ok(manager) = self.manager.upgrade() {
            WorkspaceManagerObjectData::remove_pending(&manager, Resource::id(resource));
        }

        if let Some(pos) = group.instances.iter().position(|i| i.id() == resource.id()) {
            let instance = group.instances.remove(pos);
            WorkspaceHandler::group_destroyed(state, &instance);
        }
    }
}

impl PartialEq for WorkspaceGroup {
    #[inline]
    fn eq(&self, other: &WorkspaceGroup) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for WorkspaceGroup {}

impl Hash for WorkspaceGroup {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.inner).hash(state);
    }
}

impl PartialEq for WeakWorkspaceGroup {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        std::sync::Weak::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for WeakWorkspaceGroup {}

impl Hash for WeakWorkspaceGroup {
    #[inline]
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::sync::Weak::as_ptr(&self.inner).hash(state)
    }
}

impl PartialEq<WeakWorkspaceGroup> for WorkspaceGroup {
    #[inline]
    fn eq(&self, other: &WeakWorkspaceGroup) -> bool {
        other.upgrade().map(|o| &o == self).unwrap_or(false)
    }
}

impl PartialEq<WorkspaceGroup> for WeakWorkspaceGroup {
    #[inline]
    fn eq(&self, other: &WorkspaceGroup) -> bool {
        self.upgrade().map(|o| &o == other).unwrap_or(false)
    }
}
