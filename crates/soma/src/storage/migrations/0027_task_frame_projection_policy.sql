-- Persist the TaskFrame projection policy that produced cloud_redacted_json.
--
-- Earlier TaskFrame rows stored privacy labels and blocked fields, but not the
-- policy that decided which labels were allowed. The default matches the
-- deterministic builder's project-internal cloud projection policy.

ALTER TABLE task_frames
    ADD COLUMN projection_policy_json TEXT NOT NULL DEFAULT '{"allow_project_internal":true,"allow_local_private":false}';
