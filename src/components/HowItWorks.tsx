import { Card } from "@/components/ui/card";
import { Upload, Settings, Camera, ArrowRight } from "lucide-react";

export const HowItWorks = () => {
  const steps = [
    {
      step: "01",
      icon: Upload,
      title: "Upload Your Recording",
      description: "Drag and drop any video file or record directly within the app. Supports all major video formats."
    },
    {
      step: "02", 
      icon: Settings,
      title: "Configure Loop Settings",
      description: "Set your loop preferences, adjust video quality, and customize playback options to match your needs."
    },
    {
      step: "03",
      icon: Camera,
      title: "Activate Virtual Camera",
      description: "Start the virtual camera and select 'CamLooper' as your camera source in any application."
    }
  ];

  return (
    <section id="how-it-works" className="py-24 bg-gradient-to-b from-background/50 to-background">
      <div className="container mx-auto px-4">
        <div className="text-center space-y-4 mb-16">
          <h2 className="text-3xl md:text-5xl font-bold">
            How{" "}
            <span className="bg-gradient-primary bg-clip-text text-transparent">
              CamLooper
            </span>{" "}
            Works
          </h2>
          <p className="text-xl text-muted-foreground max-w-2xl mx-auto">
            Get started in just three simple steps
          </p>
        </div>

        <div className="max-w-4xl mx-auto">
          <div className="grid md:grid-cols-3 gap-8 relative">
            {/* Connection Lines */}
            <div className="hidden md:block absolute top-24 left-1/3 right-1/3 h-0.5 bg-gradient-to-r from-primary via-accent to-primary opacity-30" />
            
            {steps.map((step, index) => (
              <div key={index} className="relative">
                <Card className="group p-8 text-center bg-gradient-card border-border hover:border-primary/40 transition-all duration-300 hover:shadow-glow">
                  {/* Step Number */}
                  <div className="absolute -top-4 left-1/2 transform -translate-x-1/2">
                    <div className="w-8 h-8 bg-primary rounded-full flex items-center justify-center text-primary-foreground font-bold text-sm shadow-glow">
                      {step.step}
                    </div>
                  </div>

                  <div className="space-y-4 pt-4">
                    <div className="relative mx-auto w-16 h-16">
                      <div className="w-16 h-16 bg-primary/10 rounded-xl flex items-center justify-center group-hover:bg-primary/20 transition-colors">
                        <step.icon className="h-8 w-8 text-primary group-hover:scale-110 transition-transform" />
                      </div>
                      <div className="absolute inset-0 bg-primary/20 rounded-xl blur-lg opacity-0 group-hover:opacity-100 transition-opacity" />
                    </div>

                    <h3 className="text-xl font-semibold group-hover:text-primary transition-colors">
                      {step.title}
                    </h3>

                    <p className="text-muted-foreground leading-relaxed">
                      {step.description}
                    </p>
                  </div>
                </Card>

                {/* Arrow for mobile */}
                {index < steps.length - 1 && (
                  <div className="md:hidden flex justify-center my-6">
                    <ArrowRight className="h-6 w-6 text-primary/60" />
                  </div>
                )}
              </div>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
};