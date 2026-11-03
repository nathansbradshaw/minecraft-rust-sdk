package dev.minecraftsdk.mixin;
import net.minecraft.world.entity.LivingEntity;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.gen.Invoker;
/** Read the effective native jump strength, including ordinary effects. */
@Mixin(LivingEntity.class)
public interface LivingPhysicsAccessor {
    @Invoker("getJumpPower") float sdk$jumpPower();
}
