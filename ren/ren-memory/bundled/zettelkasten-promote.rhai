let meta = #{
    name: "zettelkasten-promote",
    description: "Builds deterministic, reviewable ren-memory promotion proposals",
    when_to_use: "Use only through ren memory promote to propose durable notes from selected sources",
    args_schema: #{
        type: "object",
        additionalProperties: false,
        required: [
            "schema",
            "inputs",
            "nearby_notes",
            "exact_duplicates",
            "similarity_candidates",
            "graph_neighborhood",
            "policy"
        ],
        properties: #{
            schema: #{ type: "string", minLength: 1 },
            inputs: #{ type: "array" },
            nearby_notes: #{ type: "array" },
            exact_duplicates: #{ type: "array" },
            similarity_candidates: #{ type: "array" },
            graph_neighborhood: #{ type: "array" },
            policy: #{ type: "object" }
        }
    }
};

let actions = [];
for input in args.inputs {
    actions.push(#{
        action: "create_note",
        sources: [#{
            source_id: input.source_id,
            source_revision: input.source_revision
        }],
        target_id: input.target_id,
        target_type: "permanent",
        title: input.title,
        body: input.body,
        rationale: "Preserve the selected observation as a durable, source-linked note.",
        source_locators: input.sources,
        links: []
    });
}

complete(#{
    schema: "ren-memory-promotion-proposal/v2",
    actions: actions
});
