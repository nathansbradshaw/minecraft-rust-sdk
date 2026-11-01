package dev.minecraftsdk.mixin;
import net.minecraft.core.BlockPos;
import net.minecraft.server.level.ServerPlayerGameMode;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Accessor;
/** Observation only; does not alter Minecraft's mining state or protection checks. */
@Mixin(ServerPlayerGameMode.class)
public interface GameModeAccessor {
    @Accessor("isDestroyingBlock") boolean sdk$mining();
    @Accessor("destroyPos") BlockPos sdk$miningPosition();
}
