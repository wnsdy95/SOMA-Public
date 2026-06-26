-- Migration 0009 — narrative paragraph row in self_state.
--
-- Discussion 0037 §G (시나리오 C 의 50% 가치 ahead-of-schedule):
-- self_state 의 (kind='narrative', key='paragraph_md') row 가 slow_
-- loop 가 합성 한 사용자-narrative paragraph 를 보유. 가치 비교:
--
--   * pre-D90: rule-based aggregator 만 — raw fact list. Context/debug
--     profile consumers start without a semantic paragraph.
--   * post-D90/91/93: salience + decay + project_state magnitude/
--     direction. 통계 적 으로 paragraph 합성 가능.
--   * **이 migration**: slow_loop 가 1h 마다 rule-based template
--     으로 paragraph 합성 → MemoryPack 의 self_state.summary_md 가
--     채워짐. LLM 없음, frozen-weights.
--   * future v1.2 (D82 본 chunk): 같은 row 위 에 LLM-assisted
--     paragraph 가 swap-in. row schema 동일.
--
-- value_json 는 항상 동일 shape:
--   { "paragraph_md": "<markdown>", "synthesized_at_ns": <i64>, "kind": "rule" | "llm" }
--
-- 본 migration 은 row 가 비어 있 을 때 의 placeholder 만 삽입.

INSERT OR IGNORE INTO self_state (kind, key, value_json, evidence_ids, computed_at_ns)
VALUES (
    'narrative',
    'paragraph_md',
    json_object('paragraph_md', '', 'synthesized_at_ns', 0, 'kind', 'rule'),
    '[]',
    0
);
