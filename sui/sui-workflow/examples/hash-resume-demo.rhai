let meta = #{
    name: "hash-resume-demo",
    description: "Minimal workflow demonstrating content-hash resume",
    args_schema: #{
        type: "object",
        properties: #{
            topic: #{ type: "string", minLength: 1 }
        },
        required: ["topic"]
    },
    phases: [
        #{ title: "Research", detail: "Ask an agent about the topic" },
        #{ title: "Review", detail: "Review findings in parallel" }
    ]
};

phase("Research");
let answer = agent("Summarize: " + args.topic, #{
    label: "researcher",
    capability_mode: "read-only"
});

phase("Review");
let reviews = parallel([
    #{ prompt: "Correctness of: " + answer.output, label: "correctness" },
    #{ prompt: "Simplicity of: " + answer.output, label: "simplicity" }
]);

complete(#{
    answer: answer.output,
    reviews: reviews,
    fingerprint: fingerprint(args.topic),
    budget: budget()
});
