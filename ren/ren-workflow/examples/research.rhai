let meta = #{
    name: "deterministic-research",
    description: "Demonstrates deterministic agent orchestration",
    phases: [
        #{ title: "Plan", detail: "Create a plan" },
        #{ title: "Review", detail: "Review independent findings" }
    ]
};

phase("Plan");
let plan = agent("Create a concise deterministic plan", #{ label: "planner" });
log("Plan complete");

phase("Review");
let reviews = parallel([
    #{ prompt: "Review the plan for correctness", label: "correctness" },
    #{ prompt: "Review the plan for simplicity", label: "simplicity" }
]);
log("Reviews complete");

complete(#{ plan: plan, reviews: reviews, budget: budget() });
