-- Demonstrate the module contract without mutating the world.
-- Run with the Brain's `run` tool and input `examples/lua/behavior.lua`.
return {
    version = 1,
    step = function(ctx)
        if ctx.state.observed then
            return { state = ctx.state, done = true }
        end
        return {
            state = { observed = true },
            command = { tool = "observe" }
        }
    end
}
